"""Companion-layer surface over the Rust pool-updater pyclasses.

This package re-exports the Rust-owned pool-updater input pyclasses under
stable, seam-agnostic names. Driver / CLI code imports from here — never from
the PyO3 wrapper :mod:`degenbot._ffi`.

The re-exports are **direct aliases** (``from degenbot.db import X``), not
Python subclasses — the Rust engine constructs and consumes these pyclasses
directly; subclassing would break type identity at the FFI boundary.

Symbol map:
- :class:`V2PoolRowInput` — V2 pool registration input (address, factory, fee, dex variant)
- :class:`V3PoolRowInput` — V3 pool registration input (address, fee, tick spacing, dex variant)
- :class:`V4PoolRowInput` — V4 pool registration input (manager, id, fee, tick spacing, dex variant)
- :class:`LiquidityUpdateEvent` — a decoded V3/V4 liquidity update (Mint/Burn delta)
- :class:`CancelHandle` — an async cancellation token for ``run_pool_update`` / ``run_aave_update``
"""

from degenbot._ffi.cancel import CancelHandle
from degenbot.db import (
    LiquidityUpdateEvent,
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
)

__all__ = [
    "CancelHandle",
    "LiquidityUpdateEvent",
    "V2PoolRowInput",
    "V3PoolRowInput",
    "V4PoolRowInput",
]
