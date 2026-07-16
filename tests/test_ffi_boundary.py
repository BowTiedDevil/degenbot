"""Boundary test: the Pydantic ban on ``degenbot._ffi`` in non-barrier modules.

After ADR-013 (the FFI seam is private), the rule is mechanical: **no file
outside an explicit BARRIER set may import from ``degenbot._ffi``** — not
the flat root and not a typed submodule. Every production module that needs
a Rust-backed symbol imports it from its stable ``degenbot.<domain>`` home;
the barrier modules are the only files permitted to bridge ``_ffi`` → public
name.

Test code (``tests/**``) and examples (``examples/**``) are excluded from
the scan — they legitimately test the FFI seam directly.

The test is one grep: ``rg "from degenbot\\._ffi|import degenbot\\._ffi"``
over ``src/degenbot/**/*.py``, excluding the BARRIER set. A stale-barrier
guard ensures every BARRIER entry actually imports from ``_ffi`` (catches
barrier rot — a file that used to bridge but no longer does).
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCAN_DIR = REPO_ROOT / "src" / "degenbot"

# ---------------------------------------------------------------------------
# Barrier modules: the only production files permitted to import from
# ``degenbot._ffi``. Each bridges a Rust ``_ffi`` surface to a stable
# ``degenbot.<domain>`` public name.
#
# To add a barrier: the module must own a _ffi→public bridge (re-export a
# Rust pyclass/function under a stable Python name). Barriers are the seam
# between the Rust core and the Python companion layer (ADR-005/ADR-013).
# ---------------------------------------------------------------------------
BARRIER: frozenset[str] = frozenset(
    {
        # --- flat-root bridges (Py* classes re-exported under stable names) ---
        "src/degenbot/bot.py",  # PyBot, PyBotIo → Bot cockpit
        "src/degenbot/types/__init__.py",  # PyLiquidityPool, dex_identity
        "src/degenbot/arbitrage/engine_registry.py",  # UniswapArbEngine
        "src/degenbot/erc20/erc20.py",  # PyErc20Token
        "src/degenbot/exceptions/arbitrage.py",  # *RejectedError
        "src/degenbot/exceptions/verification.py",  # Verification*Error
        "src/degenbot/checksum_cache.py",  # to_checksum_address
        "src/degenbot/pathfinding.py",  # find_paths_rust, build_path_graph
        # --- typed-submodule bridges (mirror homes for _ffi.<sub>) ---
        "src/degenbot/aave/__init__.py",  # _ffi.aave (Aave price oracle)
        "src/degenbot/abi/__init__.py",  # _ffi.abi (encode/decode/decode_single)
        "src/degenbot/aerodrome/math.py",  # _ffi.solidly_math
        "src/degenbot/balancer/math.py",  # _ffi.balancer_math
        "src/degenbot/chainlink/__init__.py",  # _ffi.price (Chainlink feed)
        "src/degenbot/cli/aave.py",  # _ffi.aave (CLI driver for Aave update)
        "src/degenbot/cli/pool.py",  # _ffi.pool (CLI driver for pool update)
        "src/degenbot/contract/__init__.py",  # _ffi.contract (Contract, get_function_selector)
        "src/degenbot/curve/math.py",  # _ffi.curve_math
        "src/degenbot/db/__init__.py",  # _ffi.db (row types + db_* operations)
        "src/degenbot/dispatch/__init__.py",  # _ffi.simulation, _ffi.submission
        "src/degenbot/fork/__init__.py",  # _ffi.fork (AnvilFork)
        "src/degenbot/provider/__init__.py",  # _ffi.provider (AlloyProvider)
        "src/degenbot/uniswap/deployments.py",  # _ffi.deployments (resolve_deployer etc.)
        "src/degenbot/uniswap/math.py",  # _ffi.cl_math
        "src/degenbot/updater/__init__.py",  # _ffi.cancel, _ffi.db (updater re-exports)
        "src/degenbot/utils/solady/libzip.py",  # _ffi.solady (libzip)
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


def test_no_flat_root_ffi_imports_in_leaf_code() -> None:
    """Fail if any non-barrier file imports from ``degenbot._ffi``.

    The Pydantic ban (ADR-013): every production module that needs a
    Rust-backed symbol imports it from its stable ``degenbot.<domain>``
    home. The BARRIER set lists the only files permitted to bridge
    ``_ffi`` → public name.
    """
    violations: list[str] = []
    for f in _iter_python_files():
        rel = str(f.relative_to(REPO_ROOT))
        if rel in BARRIER:
            continue
        source = f.read_text()
        for lineno, line in enumerate(source.splitlines(), 1):
            if _FFI_IMPORT_RE.search(line):
                violations.append(f"{rel}:{lineno}: {line.strip()}")
    if violations:
        msg = (
            f"\nFound {len(violations)} `degenbot._ffi` import(s) in "
            f"non-barrier files (Pydantic ban, ADR-013):\n\n"
            + "\n".join(f"  - {v}" for v in violations)
            + "\n\nThese must import from the stable `degenbot.<domain>` "
            "home instead. See tests/test_ffi_boundary.py BARRIER for the "
            "files permitted to bridge _ffi→public name."
        )
        pytest.fail(msg)


def test_barrier_entries_actually_import_ffi() -> None:
    """Every barrier file must actually import from ``degenbot._ffi``.

    A barrier entry that DOESN'T import from ``_ffi`` is stale — it was
    rerouted to a stable home and should be removed from the BARRIER set.
    This catches barrier rot.
    """
    stale: list[str] = []
    for rel in BARRIER:
        f = REPO_ROOT / rel
        if not f.exists():
            stale.append(f"{rel} (file does not exist)")
            continue
        source = f.read_text()
        has_bridge = any(_FFI_IMPORT_RE.search(line) for line in source.splitlines())
        if not has_bridge:
            stale.append(rel)
    assert stale == [], (
        f"stale barrier entries (no `degenbot._ffi` import — remove from BARRIER): {stale}"
    )
