"""Boundary test: ``degenbot._ffi`` may only appear in ``__init__.py`` files.

The Pydantic barrier rule (ADR-013), tightened: every Rust ``_ffi`` symbol
reaches Python through a stable ``degenbot.<domain>`` home, and that home is
an ``__init__.py`` file. No leaf module (a file with real logic) may import
from ``degenbot._ffi`` — not the flat root, not a typed submodule.

This makes incomplete migrations visible: a non-``__init__.py`` file with an
``_ffi`` import is automatically suspicious, no allowlist or judgment call
required.

The 6 files in ``KNOWN_VIOLATIONS`` are category-B migration debt — leaf
modules with hundreds of lines of real logic that also bridge ``_ffi``.
Each is a TODO: extract the bridge into the package ``__init__.py``, leave
the deep logic in the leaf file importing from the home.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCAN_DIR = REPO_ROOT / "src" / "degenbot"

# ---------------------------------------------------------------------------
# Known violations: leaf modules that still bridge ``_ffi`` (category-B
# migration debt). Each entry is a repo-relative path. The fix is to extract
# the ``_ffi`` bridge into the package ``__init__.py`` and reroute the leaf
# to import from the home.
#
# To remove an entry: convert the file's ``_ffi`` import into a re-export
# in its package ``__init__.py``, then reroute the file to import from the
# home. See the category-A conversions (checksum_cache, math/, etc.) for
# the pattern.
# ---------------------------------------------------------------------------
KNOWN_VIOLATIONS: frozenset[str] = frozenset(
    {
        "src/degenbot/bot.py",  # PyBot, PyBotIo — Bot lifecycle (765 lines)
        "src/degenbot/arbitrage/engine_registry.py",  # UniswapArbEngine — engine wrapper (640 lines)
        "src/degenbot/pathfinding.py",  # find_paths_rust, build_path_graph — pathfinding (696 lines)
    }
)

# Matches any line containing an actual import from degenbot._ffi
# (flat root or typed submodule). Does NOT match docstring/comment mentions.
_FFI_IMPORT_RE = re.compile(r"(?:^|\s)(?:from|import)\s+degenbot\._ffi")


def _iter_python_files() -> list[Path]:
    """Yield every .py file under src/degenbot/ (excluding __pycache__)."""
    return [
        f
        for f in SCAN_DIR.rglob("*.py")
        if "__pycache__" not in f.parts
    ]


def test_no_ffi_imports_outside_init_files() -> None:
    """Fail if any non-``__init__.py`` file imports from ``degenbot._ffi``.

    The only files permitted to import from ``_ffi`` are ``__init__.py``
    files (the barrier modules). Every other file must import from its
    stable ``degenbot.<domain>`` home. Known violations are acknowledged
    as migration debt (see ``KNOWN_VIOLATIONS``).
    """
    violations: list[str] = []
    for f in _iter_python_files():
        rel = str(f.relative_to(REPO_ROOT))
        if rel in KNOWN_VIOLATIONS:
            continue
        if f.name == "__init__.py":
            continue
        source = f.read_text()
        for lineno, line in enumerate(source.splitlines(), 1):
            if _FFI_IMPORT_RE.search(line):
                violations.append(f"{rel}:{lineno}: {line.strip()}")
    if violations:
        msg = (
            f"\nFound {len(violations)} `degenbot._ffi` import(s) in "
            f"non-`__init__.py` files (ADR-013: the Pydantic barrier):\n\n"
            + "\n".join(f"  - {v}" for v in violations)
            + "\n\nThese must import from the stable `degenbot.<domain>` "
            "home instead. See tests/test_ffi_boundary.py for the rule."
        )
        pytest.fail(msg)


def test_known_violations_still_exist() -> None:
    """Every KNOWN_VIOLATIONS entry must still import from ``_ffi``.

    A violation that no longer imports from ``_ffi`` has been migrated
    and should be removed from the set. This catches stale debt entries.
    """
    migrated: list[str] = []
    for rel in KNOWN_VIOLATIONS:
        f = REPO_ROOT / rel
        if not f.exists():
            migrated.append(f"{rel} (file removed)")
            continue
        source = f.read_text()
        has_ffi = any(_FFI_IMPORT_RE.search(line) for line in source.splitlines())
        if not has_ffi:
            migrated.append(rel)
    assert migrated == [], (
        f"KNOWN_VIOLATIONS entries that no longer import from `_ffi` "
        f"(remove them — migration complete): {migrated}"
    )
