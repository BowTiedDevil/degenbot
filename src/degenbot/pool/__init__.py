"""Pool updater operations — the stable mirror home for ``_ffi.pool``.

Bridges the Rust ``degenbot-pool-updater`` core crate. The functions are
thin PyO3 wrappers: ``run_pool_update`` drives the per-pool chunk loop,
``verify_v3_liquidity_map`` / ``verify_v4_liquidity_map`` verify decoded
liquidity against on-chain truth.
"""

from degenbot._ffi.pool import (
    run_pool_update,
    verify_v3_liquidity_map,
    verify_v4_liquidity_map,
)

__all__ = [
    "run_pool_update",
    "verify_v3_liquidity_map",
    "verify_v4_liquidity_map",
]
