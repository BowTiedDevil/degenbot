from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS
from degenbot.checksum_cache import get_checksum_address

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import PancakeswapV2Pool, PancakeswapV3Pool
from .trackers import PancakeswapV2PoolTracker, PancakeswapV3PoolTracker


def _register_pancakeswap_deployments() -> None:
    v2_factories = [
        (1, "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"),
        (8453, "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"),
    ]
    v3_factories = [
        (1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
        (8453, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"),
    ]

    for chain_id, factory in v2_factories:
        deployment = FACTORY_DEPLOYMENTS.get(chain_id, {}).get(get_checksum_address(factory))
        pool_type_registry.register(
            PancakeswapV2Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=deployment.pool_init_hash or None if deployment else None,
            deployer=deployment.deployer if deployment and deployment.deployer else None,
        )

    for chain_id, factory in v3_factories:
        deployment = FACTORY_DEPLOYMENTS.get(chain_id, {}).get(get_checksum_address(factory))
        pool_type_registry.register(
            PancakeswapV3Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=deployment.pool_init_hash or None if deployment else None,
            deployer=deployment.deployer if deployment and deployment.deployer else None,
        )


_register_pancakeswap_deployments()

__all__ = (
    "PancakeswapV2Pool",
    "PancakeswapV2PoolTracker",
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
)
