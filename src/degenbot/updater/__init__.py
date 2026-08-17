"""Pool-updater surface — the single stable home for pool-updater ops and types.

Everything a driver needs from the Rust pool-updater machinery:

- :func:`run_pool_update` — drives the per-pool chunk loop (Rust core)
- :func:`verify_v3_liquidity_map` / :func:`verify_v4_liquidity_map` — verify
  decoded liquidity against on-chain truth
- :class:`V2PoolRowInput` / :class:`V3PoolRowInput` / :class:`V4PoolRowInput`
  — pool registration inputs
- :class:`LiquidityUpdateEvent` — a decoded V3/V4 liquidity update (Mint/Burn delta)
- :class:`CancelHandle` — an async cancellation token for ``run_pool_update`` /
  ``run_aave_update``

The re-exports are **direct aliases** of the ``degenbot._ffi`` pyclasses and
functions — never Python subclasses: the Rust engine constructs and consumes
these pyclasses directly, so subclassing would break type identity at the FFI
boundary. (I4H7EH: this package is the single home — the previously misleadingly
named pool-mirror package was deleted, and ``degenbot.db`` no longer claims
the row-input/event types.)
"""

from degenbot._ffi.cancel import CancelHandle
from degenbot._ffi.db import (
    LiquidityUpdateEvent,
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
)
from degenbot._ffi.pool import (
    run_pool_update,
    verify_v3_liquidity_map,
    verify_v4_liquidity_map,
)

__all__ = [
    "CancelHandle",
    "LiquidityUpdateEvent",
    "V2PoolRowInput",
    "V3PoolRowInput",
    "V4PoolRowInput",
    "run_pool_update",
    "verify_v3_liquidity_map",
    "verify_v4_liquidity_map",
]
