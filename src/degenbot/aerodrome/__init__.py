"""Aerodrome V2/V3 liquidity pools and trackers."""

from degenbot.registry.pool_type import pool_type_registry

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import AerodromeV2Pool, AerodromeV3Pool
from .trackers import AerodromeV2PoolTracker, AerodromeV3PoolTracker
from .types import AerodromeV2PoolState, AerodromeV3PoolState

# Register Aerodrome factories with the unified pool type registry.
# Deployment data previously sourced from FACTORY_DEPLOYMENTS.
# Now hardcoded directly — pool_type_registry is the sole source of truth.

_v3_factory_address = "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"
_v3_chain_id = 8453
pool_type_registry.register(
    AerodromeV3Pool,
    chain_id=_v3_chain_id,
    factory_address=_v3_factory_address,
    pool_init_hash="",  # Aerodrome V3 uses a different address generation
    deployer=None,
)

# Register Aerodrome V2 factory with the unified pool type registry.
# AerodromeV2Pool has a non-standard constructor that requires `stable` and `fee`
# arguments. The builder handles the chain fetches for these.
_v2_factory_address = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"
_v2_chain_id = 8453
pool_type_registry.register(
    AerodromeV2Pool,
    chain_id=_v2_chain_id,
    factory_address=_v2_factory_address,
    pool_init_hash="",  # Aerodrome V2 uses a different address generation
    deployer=None,
)

__all__ = (
    "AerodromeV2Pool",
    "AerodromeV2PoolState",
    "AerodromeV2PoolTracker",
    "AerodromeV3Pool",
    "AerodromeV3PoolState",
    "AerodromeV3PoolTracker",
)
