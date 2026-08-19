"""Uniswap V2/V3/V4 pool type registration and exports."""

# Deployment resolver functions — bridged from the Rust ``degenbot-core``
# deployments resolver (ADR-013: ``_ffi`` only in ``__init__.py`` barrier).
from degenbot._ffi.deployments import (
    resolve_deployer,
    resolve_v2_init_hash,
    resolve_v3_init_hash,
)

# Set default pool classes — UniswapV2Pool and UniswapV3Pool serve as the
# fallback when no factory-specific registration exists.
# Deployment data (chain_id, factory → deployer / init_hash / variant /
# dex_identity) is loaded from the shipped deployments.json by the top-level
# `degenbot` package init via `register_from_deployments(load_deployments())`.
from degenbot.registry.pool_type import pool_type_registry

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .trackers import UniswapV2PoolTracker, UniswapV3PoolTracker
from .v2_liquidity_pool import UniswapV2Pool
from .v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from .v3_liquidity_pool import UniswapV3Pool
from .v3_snapshot import UniswapV3LiquiditySnapshot
from .v3_types import (
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)
from .v4_liquidity_pool import UniswapV4Pool
from .v4_snapshot import UniswapV4LiquiditySnapshot
from .v4_types import UniswapV4PoolExternalUpdate, UniswapV4PoolState

pool_type_registry.set_default_v2_class(UniswapV2Pool)
pool_type_registry.set_default_v3_class(UniswapV3Pool)


__all__ = (
    "UniswapV2Pool",
    "UniswapV2PoolExternalUpdate",
    "UniswapV2PoolSimulationResult",
    "UniswapV2PoolState",
    "UniswapV2PoolTracker",
    "UniswapV3LiquiditySnapshot",
    "UniswapV3Pool",
    "UniswapV3PoolExternalUpdate",
    "UniswapV3PoolSimulationResult",
    "UniswapV3PoolState",
    "UniswapV3PoolTracker",
    "UniswapV4LiquiditySnapshot",
    "UniswapV4Pool",
    "UniswapV4PoolExternalUpdate",
    "UniswapV4PoolState",
    "resolve_deployer",
    "resolve_v2_init_hash",
    "resolve_v3_init_hash",
)
