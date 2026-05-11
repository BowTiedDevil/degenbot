from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import CamelotLiquidityPool

__all__ = ("CamelotLiquidityPool",)

# Register with the unified pool type registry
import eth_typing

from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS, ArbitrumCamelotV2

_factory_address = ArbitrumCamelotV2.factory.address
_chain_id = eth_typing.ChainId.ARB1
_deployment = FACTORY_DEPLOYMENTS.get(_chain_id, {}).get(_factory_address)
pool_type_registry.register(
    CamelotLiquidityPool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash=_deployment.pool_init_hash or None if _deployment else None,
    deployer=_deployment.deployer if _deployment and _deployment.deployer else None,
)
