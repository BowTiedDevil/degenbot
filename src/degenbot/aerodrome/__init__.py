from degenbot.checksum_cache import get_checksum_address
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import AerodromeV2Pool, AerodromeV3Pool
from .trackers import AerodromeV2PoolTracker, AerodromeV3PoolTracker
from .types import AerodromeV2PoolState, AerodromeV3PoolState

# Register Aerodrome V3 factory with the unified pool type registry.
_v3_factory_address = "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"
_chain_id = 8453
_v3_deployment = FACTORY_DEPLOYMENTS.get(_chain_id, {}).get(get_checksum_address(_v3_factory_address))
pool_type_registry.register(
    AerodromeV3Pool,
    chain_id=_chain_id,
    factory_address=_v3_factory_address,
    pool_init_hash=_v3_deployment.pool_init_hash or None if _v3_deployment else None,
    deployer=_v3_deployment.deployer if _v3_deployment and _v3_deployment.deployer else None,
)

# Register Aerodrome V2 factory with the unified pool type registry.
# AerodromeV2Pool has a non-standard constructor that requires `stable` and `fee`
# arguments. The builder handles the chain fetches for these.
_v2_factory_address = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"
_v2_deployment = FACTORY_DEPLOYMENTS.get(_chain_id, {}).get(get_checksum_address(_v2_factory_address))
pool_type_registry.register(
    AerodromeV2Pool,
    chain_id=_chain_id,
    factory_address=_v2_factory_address,
    pool_init_hash=_v2_deployment.pool_init_hash or None if _v2_deployment else None,
    deployer=_v2_deployment.deployer if _v2_deployment and _v2_deployment.deployer else None,
)

__all__ = (
    "AerodromeV2Pool",
    "AerodromeV2PoolState",
    "AerodromeV2PoolTracker",
    "AerodromeV3Pool",
    "AerodromeV3PoolState",
    "AerodromeV3PoolTracker",
)
