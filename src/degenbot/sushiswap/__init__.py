"""SushiSwap V2/V3 pool type registration and exports."""

from degenbot.degenbot_rs import dex_identity
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import SushiswapV3Pool
from .trackers import SushiswapV3PoolTracker


# Register Sushiswap V2 and V3 factories with the unified pool type registry.
# Deployment data previously sourced from FACTORY_DEPLOYMENTS.
# Now hardcoded directly — pool_type_registry is the sole source of truth.
def _register_sushiswap_deployments() -> None:
    v2_deployments = [
        (
            1,
            "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
            None,
        ),
        (
            8453,
            "0x71524B4f93c58fcbF659783284E38825f0622859",
            "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
            None,
        ),
        (
            42161,
            "0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
            "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
            None,
        ),
    ]
    v3_deployments = [
        (
            1,
            "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
            "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
            None,
        ),
        (
            8453,
            "0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
            "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
            None,
        ),
        (
            42161,
            "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e",
            "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
            None,
        ),
    ]

    sushiswap_v2_identity = dex_identity("sushiswap-v2")
    assert sushiswap_v2_identity is not None, "sushiswap-v2 preset must resolve"
    for chain_id, factory, init_hash, deployer in v2_deployments:
        pool_type_registry.register(
            LiquidityPool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
            variant="sushiswap",
            dex_identity=sushiswap_v2_identity,
        )

    for chain_id, factory, init_hash, deployer in v3_deployments:
        pool_type_registry.register(
            SushiswapV3Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
        )


_register_sushiswap_deployments()

__all__ = (
    "SushiswapV3Pool",
    "SushiswapV3PoolTracker",
    "UniswapV3Pool",
)
