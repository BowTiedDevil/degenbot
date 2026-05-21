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
    UniswapV2PoolStateUpdated,
)
from .v3_liquidity_pool import UniswapV3Pool
from .v3_snapshot import UniswapV3LiquiditySnapshot
from .v3_types import (
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from .v4_liquidity_pool import UniswapV4Pool
from .v4_snapshot import UniswapV4LiquiditySnapshot
from .v4_types import UniswapV4PoolExternalUpdate, UniswapV4PoolState, UniswapV4PoolStateUpdated

# Register default pool classes — UniswapV2Pool and UniswapV3Pool serve as the
# fallback when no factory-specific registration exists.
pool_type_registry.set_default_v2_class(UniswapV2Pool)
pool_type_registry.set_default_v3_class(UniswapV3Pool)


def _register_uniswap_deployments() -> None:
    # Deployment data previously sourced from FACTORY_DEPLOYMENTS.
    # Now hardcoded directly — pool_type_registry is the sole source of truth.
    v2_deployments = [
        (1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f", "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f", None),  # noqa: E501
        (8453, "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6", "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f", None),  # noqa: E501
    ]
    v3_deployments = [
        (1, "0x1F98431c8aD98523631AE4a59f267346ea31F984", "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54", None),  # noqa: E501
        (8453, "0x33128a8fC17869897dcE68Ed026d694621f6FDfD", "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54", None),  # noqa: E501
        (42161, "0x1F98431c8aD98523631AE4a59f267346ea31F984", "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54", None),  # noqa: E501
    ]

    for chain_id, factory, init_hash, deployer in v2_deployments:
        pool_type_registry.register(
            UniswapV2Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
        )

    for chain_id, factory, init_hash, deployer in v3_deployments:
        pool_type_registry.register(
            UniswapV3Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
        )


_register_uniswap_deployments()

__all__ = (
    "UniswapV2Pool",
    "UniswapV2PoolExternalUpdate",
    "UniswapV2PoolSimulationResult",
    "UniswapV2PoolState",
    "UniswapV2PoolStateUpdated",
    "UniswapV2PoolTracker",
    "UniswapV3LiquiditySnapshot",
    "UniswapV3Pool",
    "UniswapV3PoolExternalUpdate",
    "UniswapV3PoolSimulationResult",
    "UniswapV3PoolState",
    "UniswapV3PoolStateUpdated",
    "UniswapV3PoolTracker",
    "UniswapV4LiquiditySnapshot",
    "UniswapV4Pool",
    "UniswapV4PoolExternalUpdate",
    "UniswapV4PoolState",
    "UniswapV4PoolStateUpdated",
)
