"""Boundary test: ban flat-root ``degenbot._ffi`` imports in leaf code.

After the three-layer transition (ADR-005) and the flat→submodule conversion
epic (XZ54NW) + the companion-homes remap (WLAB6U), the only modules that may
import symbols from the flat root ``degenbot._ffi`` grab-bag are the companion
re-exporters themselves (which own the _ffi→companion bridge). Every other
module must import from the typed companion home or a typed ``_ffi`` submodule.

This test AST-scans every ``src/degenbot/**/*.py``, ``tests/**/*.py``, and
``examples/**/*.py`` file and FAILs if any file outside an explicit allowlist
contains:

  - ``from degenbot._ffi import <Name>``  — a flat-root symbol import, OR
  - ``from degenbot import _ffi``         — importing the root package itself, OR
  - ``import degenbot._ffi``              — same, in `import` form.

Submodule imports (``from degenbot._ffi.db import X``,
``from degenbot._ffi.cl_math import muldiv``, ``from degenbot._ffi import
executor`` where ``executor`` is a submodule) are NOT banned — the typed,
namespaced surface is the approved destination.
"""

from __future__ import annotations

import ast
import importlib
import inspect
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCAN_DIRS = (
    REPO_ROOT / "src" / "degenbot",
    REPO_ROOT / "tests",
    REPO_ROOT / "examples",
)

# ---------------------------------------------------------------------------
# Allowlist: files permitted to import from flat-root ``degenbot._ffi``.
#
# Each entry is a repo-relative path. Add a companion re-exporter here only
# when it owns the _ffi→companion bridge for its domain. Add the reason as a
# comment — the test failure message names the file so the reason should be
# discoverable.
# ---------------------------------------------------------------------------
ALLOWLIST: frozenset[str] = frozenset(
    {
        # --- companion re-exporters (own the _ffi→companion bridge) ---
        "src/degenbot/bot.py",  # PyBot + PyBotIo: the bot cockpit
        "src/degenbot/types/__init__.py",  # PyLiquidityPool: DEX-agnostic pool handle
        "src/degenbot/arbitrage/engine_registry.py",  # UniswapArbEngine: engine wrapper
        "src/degenbot/erc20/erc20.py",  # PyErc20Token: ERC20 companion
        "src/degenbot/exceptions/arbitrage.py",  # *RejectedError: exception re-exporter
        "src/degenbot/exceptions/verification.py",  # Verification*Error: exception re-exporter
        "src/degenbot/checksum_cache.py",  # to_checksum_address: checksum module
        "src/degenbot/pathfinding.py",  # find_paths_rust, build_path_graph: pathfinding
        # --- test files that introspect the _ffi root package itself ---
        "tests/rust/test_pyclass_module_annotation.py",  # walks dir(_ffi) to assert __module__
    }
)


def _ffi_submodule_names() -> frozenset[str]:
    """Return the set of real submodules of ``degenbot._ffi`` at runtime.

    Used to distinguish ``from degenbot._ffi import <submod>`` (a submodule
    import — allowed) from ``from degenbot._ffi import <Symbol>`` (a flat-root
    symbol import — banned).
    """
    try:
        ffi = importlib.import_module("degenbot._ffi")
    except ImportError:
        return frozenset()
    return frozenset(
        name
        for name in dir(ffi)
        if not name.startswith("_")
        and inspect.ismodule(getattr(ffi, name))
    )


_FFI_SUBMODULES = _ffi_submodule_names()


def _iter_python_files() -> list[tuple[Path, str]]:
    """Yield (absolute_path, repo_relative_path) for every .py file in scan dirs."""
    out: list[tuple[Path, str]] = []
    for base in SCAN_DIRS:
        for f in base.rglob("*.py"):
            if "__pycache__" in f.parts:
                continue
            if f.is_absolute():
                rel = str(f.relative_to(REPO_ROOT))
            else:
                rel = str(f)
                f = REPO_ROOT / f
            out.append((f, rel))
    return out


def _find_banned_imports(
    source: str,
    rel_path: str,
    ffi_submodules: frozenset[str],
) -> list[str]:
    """Return a list of human-readable violation strings for ``source``.

    A violation is any ``from degenbot._ffi import <Name>`` where ``<Name>`` is
    NOT a submodule, or any ``from degenbot import _ffi`` / ``import
    degenbot._ffi`` (importing the root package for symbol access).
    """
    violations: list[str] = []
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return violations
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level == 0:
            if node.module == "degenbot._ffi":
                # from degenbot._ffi import X, Y
                for alias in node.names:
                    if alias.name in ffi_submodules:
                        continue  # submodule import — allowed
                    as_str = f" as {alias.asname}" if alias.asname else ""
                    violations.append(
                        f"{rel_path}:{node.lineno}: "
                        f"from degenbot._ffi import {alias.name}{as_str} "
                        f"— import from the typed companion home instead "
                        f"(see tests/test_ffi_boundary.py ALLOWLIST)"
                    )
            elif node.module == "degenbot" and any(
                a.name == "_ffi" for a in node.names
            ):
                # from degenbot import _ffi
                violations.append(
                    f"{rel_path}:{node.lineno}: from degenbot import _ffi "
                    f"— import from the typed companion home instead"
                )
        elif isinstance(node, ast.Import):
            # import degenbot._ffi [as X] — root package access
            for alias in node.names:
                if alias.name == "degenbot._ffi":
                    as_str = f" as {alias.asname}" if alias.asname else ""
                    violations.append(
                        f"{rel_path}:{node.lineno}: "
                        f"import degenbot._ffi{as_str} "
                        f"— import from the typed companion home instead"
                    )
    return violations


def test_no_flat_root_ffi_imports_in_leaf_code() -> None:
    """Fail if any non-allowlisted file imports from flat-root ``degenbot._ffi``.

    Companion re-exporters (the ALLOWLIST) own the _ffi→companion bridge and
    are the only files permitted to reach into the flat root. Everyone else
    must import from the typed companion (``degenbot.bot``, ``degenbot.types``,
    ``degenbot.exceptions``, etc.) or a typed ``_ffi`` submodule
    (``degenbot._ffi.db``, ``degenbot._ffi.cl_math``).
    """
    ffi_submodules = _ffi_submodule_names()
    violations: list[str] = []
    files_scanned = 0
    allowlisted_hits = 0
    for f, rel in _iter_python_files():
        files_scanned += 1
        if rel in ALLOWLIST:
            allowlisted_hits += 1
            continue
        source = f.read_text()
        violations.extend(_find_banned_imports(source, rel, ffi_submodules))
    if violations:
        msg = (
            f"\nFound {len(violations)} flat-root `degenbot._ffi` import(s) in "
            f"non-allowlisted files (scanned {files_scanned} .py files, "
            f"{allowlisted_hits} allowlisted):\n\n"
            + "\n".join(f"  - {v}" for v in violations)
            + "\n\nThese must be remapped to the typed companion home or a "
            "typed _ffi submodule. See tests/test_ffi_boundary.py ALLOWLIST for "
            "the files permitted to bridge _ffi→companion."
        )
        pytest.fail(msg)


def test_submodule_imports_are_not_banned() -> None:
    """Guard: ``from degenbot._ffi.<sub> import X`` must NOT trip the ban.

    This is a positive control: if the boundary test logic regresses to ban
    submodule imports too, this test catches it.
    """
    # Build a fake source with a definitely-allowed submodule import.
    # degenbot._ffi.db is a real submodule (shipped in every build).
    ffi_submodules = _ffi_submodule_names()
    assert "db" in ffi_submodules, (
        "degenbot._ffi.db must be a real submodule (sanity: the conversion "
        "epic shipped it)"
    )
    fake_source = "from degenbot._ffi import db\n"
    violations = _find_banned_imports(fake_source, "<fake>", ffi_submodules)
    assert violations == [], (
        f"submodule import `from degenbot._ffi import db` must not be banned, "
        f"got: {violations}"
    )


def test_allowlist_entries_exist() -> None:
    """Every allowlisted path must exist (stale allowlist entries are noise)."""
    missing = [p for p in ALLOWLIST if not (REPO_ROOT / p).exists()]
    assert missing == [], (
        f"stale allowlist entries (file no longer exists): {missing}"
    )


def test_allowlist_entries_actually_import_ffi_root() -> None:
    """Every allowlisted file must actually contain a flat-root _ffi import.

    A file in the allowlist that DOESN'T import from _ffi root is dead weight
    — it should be removed from the allowlist. This catches allowlist rot.
    """
    ffi_submodules = _ffi_submodule_names()
    stale: list[str] = []
    for rel in ALLOWLIST:
        f = REPO_ROOT / rel
        if not f.exists():
            continue  # covered by the existence test
        source = f.read_text()
        # An allowlisted file should have at least one flat-root import that is
        # NOT a pure submodule import (i.e. it bridges a real symbol).
        tree = ast.parse(source)
        has_root_bridge = False
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.level == 0:
                if node.module == "degenbot._ffi" and any(
                    a.name not in ffi_submodules for a in node.names
                ):
                    has_root_bridge = True
                    break
                if node.module == "degenbot" and any(
                    a.name == "_ffi" for a in node.names
                ):
                    has_root_bridge = True
                    break
            elif isinstance(node, ast.Import) and any(
                a.name == "degenbot._ffi" for a in node.names
            ):
                has_root_bridge = True
                break
        if not has_root_bridge:
            stale.append(rel)
    assert stale == [], (
        f"allowlist entries with no flat-root _ffi bridge (remove them): {stale}"
    )
