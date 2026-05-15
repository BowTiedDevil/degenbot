from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import SushiswapV2Pool, SushiswapV3Pool
from .trackers import SushiswapV2PoolTracker, SushiswapV3PoolTracker


# Register Sushiswap V2 and V3 factories with the unified pool type registry.
# Deployment data (deployer, init_hash) is sourced from FACTORY_DEPLOYMENTS.
def _register_sushiswap_deployments() -> None:
    v2_factories = [
        (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"),
        (8453, "0x71524B4f93c58fcbF659783284E38825f0622859"),
        (42161, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
    ]
    v3_factories = [
        (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        (8453, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"),
        (42161, "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"),
    ]

    for chain_id, factory in v2_factories:
        deployment = FACTORY_DEPLOYMENTS.get(chain_id, {}).get(factory)
        pool_type_registry.register(
            SushiswapV2Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=deployment.pool_init_hash or None if deployment else None,
            deployer=deployment.deployer if deployment and deployment.deployer else None,
        )

    for chain_id, factory in v3_factories:
        deployment = FACTORY_DEPLOYMENTS.get(chain_id, {}).get(factory)
        pool_type_registry.register(
            SushiswapV3Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=deployment.pool_init_hash or None if deployment else None,
            deployer=deployment.deployer if deployment and deployment.deployer else None,
        )


_register_sushiswap_deployments()

__all__ = (
    "SushiswapV2Pool",
    "SushiswapV2PoolTracker",
    "SushiswapV3Pool",
    "SushiswapV3PoolTracker",
)
