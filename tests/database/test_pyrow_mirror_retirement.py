"""The `Py*Row` records are core-owned, not Python mirrors.

ADR-023 D2 kept `Erc20TokenRow` + the six `db/pool_read.py` classes as Python

mirrors of `degenbot-db`'s row records. They are gone: the four still serving as

the DB read shape are `PyO3` classes over `degenbot-db` typed records

re-exported through the stable `degenbot.db` path. This gate pins that ownership,

which present code could silently lose by re-introducing a Python class.
"""

from __future__ import annotations

import degenbot.db as public_db

# The subset of the retired mirror names still exposed to Python, each as a Rust

# `#[pyclass]` over the `degenbot-db` typed record (not a Python class).

CORE_OWNED_RECORD_NAMES = frozenset({
    "Erc20TokenRow",
    "LiquidityPoolRow",
    "ExchangeRow",
    "PoolManagerRow",
})

# The only modules a `degenbot-db`-owned record may announce as its home.

FFI_MODULES = frozenset({"degenbot._ffi", "degenbot._ffi.db"})

# live-invariant gate (ADR-087 D2): watches the live `public_db` record classes, which

# must stay Rust-owned `#[pyclass]` types on the `degenbot._ffi` seam; not

# tripwire precedent.


def test_surviving_records_are_rust_owned_pyclasses() -> None:

    for name in sorted(CORE_OWNED_RECORD_NAMES):
        record = getattr(public_db, name)

        assert record.__module__ in FFI_MODULES, (name, record.__module__)
