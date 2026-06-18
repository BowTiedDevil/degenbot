"""PancakeSwap V2/V3 pool type registration."""

from degenbot.degenbot_rs import dex_identity
from degenbot.registry.pool_type import pool_type_registry

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import PancakeswapV2Pool, PancakeswapV3Pool
from .trackers import PancakeswapV2PoolTracker, PancakeswapV3PoolTracker


def _register_pancakeswap_deployments() -> None:
    v2_deployments = [
        (
            1,
            "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362",
            "0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d",
            None,
        ),
        (
            8453,
            "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E",
            "0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d",
            None,
        ),
    ]
    v3_deployments = [
        (
            1,
            "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
            "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
            "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
        ),
        (
            8453,
            "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
            "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2",
            "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
        ),
    ]

    pancakeswap_v2_identity = dex_identity("pancakeswap-v2")
    assert pancakeswap_v2_identity is not None, "pancakeswap-v2 preset must resolve"
    for chain_id, factory, init_hash, deployer in v2_deployments:
        pool_type_registry.register(
            PancakeswapV2Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
            dex_identity=pancakeswap_v2_identity,
        )

    for chain_id, factory, init_hash, deployer in v3_deployments:
        pool_type_registry.register(
            PancakeswapV3Pool,
            chain_id=chain_id,
            factory_address=factory,
            pool_init_hash=init_hash,
            deployer=deployer,
        )


_register_pancakeswap_deployments()

__all__ = (
    "PancakeswapV2Pool",
    "PancakeswapV2PoolTracker",
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
)
