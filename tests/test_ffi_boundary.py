"""Boundary test: ``degenbot._ffi`` may only appear in ``__init__.py`` files.

The Pydantic barrier rule (ADR-013): every Rust ``_ffi`` symbol reaches
Python through a stable ``degenbot.<domain>`` home, and that home is an
``__init__.py`` file. No leaf module (a file with real logic) may import
from ``degenbot._ffi`` — not the flat root, not a typed submodule.

This makes incomplete migrations visible: a non-``__init__.py`` file with an
``_ffi`` import is automatically suspicious, no allowlist required.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCAN_DIR = REPO_ROOT / "src" / "degenbot"

# Matches any line containing an actual import from degenbot._ffi
# (flat root or typed submodule). Does NOT match docstring/comment mentions.
_FFI_IMPORT_RE = re.compile(r"(?:^|\s)(?:from|import)\s+degenbot\._ffi")

# ADR-032 fork (module-path disambiguation): the engine-handle pyclasses
# may be imported directly from _ffi by first-party code. Their clean names
# collide with same-named Python driver/model classes, and the module path
# (degenbot._ffi vs the domain home) is the disambiguator.
ENGINE_HANDLES: frozenset[str] = frozenset({
    "Bot",
    "BotIo",
    "Erc20Token",
    "DatabasePositionQuery",
    "DatabaseSnapshot",
})

_FFI_NAMES_RE = re.compile(r"^\s*from\s+degenbot\._ffi(?:\.[a-z_]+)?\s+import\s+(.+)$")


def _iter_python_files() -> list[Path]:
    """Yield every .py file under src/degenbot/ (excluding __pycache__)."""
    return [f for f in SCAN_DIR.rglob("*.py") if "__pycache__" not in f.parts]


def test_no_ffi_imports_outside_init_files() -> None:
    """Fail if any non-``__init__.py`` file imports from ``degenbot._ffi``.

    The only files permitted to import from ``_ffi`` are ``__init__.py``
    files (the barrier modules). Every other file must import from its
    stable ``degenbot.<domain>`` home.
    """
    violations: list[str] = []
    for f in _iter_python_files():
        if f.name == "__init__.py":
            continue
        rel = str(f.relative_to(REPO_ROOT))
        source = f.read_text()
        for lineno, line in enumerate(source.splitlines(), 1):
            if not _FFI_IMPORT_RE.search(line):
                continue
            m = _FFI_NAMES_RE.match(line)
            if m:
                names = {n.split(" as ")[0].strip() for n in m.group(1).split(",") if n.strip()}
                if names and names <= ENGINE_HANDLES:
                    continue  # exempt: engine-handle types (ADR-032 fork)
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
