from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS

from .managers import SwapbasedV2PoolManager
from .pools import SwapbasedV2Pool

# Register Swapbased V2 factory
_factory_address = "0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"
_chain_id = 8453
_deployment = FACTORY_DEPLOYMENTS.get(_chain_id, {}).get(_factory_address)
pool_type_registry.register(
    SwapbasedV2Pool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash=_deployment.pool_init_hash or None if _deployment else None,
    deployer=_deployment.deployer if _deployment and _deployment.deployer else None,
)

__all__ = (
    "SwapbasedV2Pool",
    "SwapbasedV2PoolManager",
)
