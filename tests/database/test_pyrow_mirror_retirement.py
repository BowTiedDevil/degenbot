"""PC6 acceptance: the `Py*Row` mirrors are retired; the records are core-owned.

ADR-023 D2 kept `Erc20TokenRow` + the six `db/pool_read.py` classes as Python
mirrors of `degenbot-db`'s row records. They are gone: no Python class under
`src/degenbot/` carries those names, and the four still serving as the DB read
shape are `PyO3` classes over `degenbot-db` typed records re-exported through the
stable `degenbot.db` path. A reintroduced Python mirror fails here.
"""

from __future__ import annotations

import ast
from pathlib import Path

import degenbot.db as public_db

# `tests/database/` -> `tests/` -> the repo root; `parents[2]`, not `parents[1]`.
REPO_ROOT = Path(__file__).resolve().parents[2]
SRC_ROOT = REPO_ROOT / "src" / "degenbot"

# ADR-023 D2's seven mirrors: `Erc20TokenRow` + the six `db/pool_read.py` classes.
RETIRED_MIRROR_NAMES = frozenset({
    "Erc20TokenRow",
    "LiquidityPoolRow",
    "PoolKindRow",
    "ExchangeRow",
    "PoolManagerRow",
    "LiquidityPositionRow",
    "InitializationMapRow",
})

# The subset of those names still exposed to Python, each as a Rust `#[pyclass]`
# over the `degenbot-db` typed record (not a Python class).
CORE_OWNED_RECORD_NAMES = frozenset({
    "Erc20TokenRow",
    "LiquidityPoolRow",
    "ExchangeRow",
    "PoolManagerRow",
})

# The only modules a `degenbot-db`-owned record may announce as its home.
FFI_MODULES = frozenset({"degenbot._ffi", "degenbot._ffi.db"})


def _python_class_locations(root: Path) -> dict[str, list[str]]:
    """Map every class name defined in a `.py` file under `root` to its files.

    Every `.py` module under `root` is scanned, however it is named and however
    deep it sits, so a freshly added module cannot slip past on a naming or
    directory filter. `.pyi` stubs are excluded on purpose: they declare the FFI
    surface of the Rust-owned types, which is exactly the residue this test
    allows.
    """
    if not root.is_dir():
        msg = f"scan root does not exist: {root}"
        raise AssertionError(msg)
    locations: dict[str, list[str]] = {}
    scanned = 0
    for path in sorted(root.rglob("*.py")):
        scanned += 1
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.ClassDef):
                locations.setdefault(node.name, []).append(str(path.relative_to(root)))
    if scanned == 0:
        msg = f"scan root matched no modules; the gate would be inert: {root}"
        raise AssertionError(msg)
    return locations


def test_no_python_class_mirrors_a_retired_db_row() -> None:
    defined = _python_class_locations(SRC_ROOT)
    # Positive control: a vacuous scan (bad root, wrong glob, empty tree) must
    # fail loudly here rather than let the retirement claim pass on nothing.
    assert defined, f"no Python classes found under {SRC_ROOT}; scan is inert"
    reintroduced = {name: defined[name] for name in sorted(defined) if name in RETIRED_MIRROR_NAMES}
    assert not reintroduced, f"Python row mirrors reintroduced under src/: {reintroduced}"


# live-invariant gate (ADR-087 D2): watches the live `public_db` record classes, which
# must stay Rust-owned `#[pyclass]` types on the `degenbot._ffi` seam; not
# tripwire precedent.
def test_surviving_records_are_rust_owned_pyclasses() -> None:
    for name in sorted(CORE_OWNED_RECORD_NAMES):
        record = getattr(public_db, name)
        assert record.__module__ in FFI_MODULES, (name, record.__module__)
