"""
Tests for exchange registration via pool_type_registry.

Replaces the former FACTORY_DEPLOYMENTS/register_exchange tests.
pool_type_registry is now the sole source of truth for deployment data.
"""

import random

import pytest
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


def _generate_random_address() -> ChecksumAddress:
    return get_checksum_address(random.randbytes(20))


def test_pool_type_registry_is_source_of_truth() -> None:
    """pool_type_registry has registrations for all built-in factory deployments."""
    # All V2/V3 DEX factories should be registered in the singleton
    # Spot-check a few well-known factories
    assert pool_type_registry.has_registration(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")  # Uniswap V2
    assert pool_type_registry.has_registration(1, "0x1F98431c8aD98523631AE4a59f267346ea31F984")  # Uniswap V3
    assert pool_type_registry.has_registration(1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")  # Sushiswap V2
    assert pool_type_registry.has_registration(8453, "0x420DD381b31aEf6683db6B902084cB0FFECe40Da")  # Aerodrome V2
    assert pool_type_registry.has_registration(42161, "0x6EcCab422D763aC031210895C81787E87B43A652")  # Camelot V2


def test_deployment_data_has_correct_init_hashes() -> None:
    """Deployment data in pool_type_registry has correct pool_init_hash values."""
    # Uniswap V2 mainnet
    dep = pool_type_registry.get_deployment(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    assert dep is not None
    assert dep.pool_init_hash == "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"

    # Uniswap V3 mainnet
    dep = pool_type_registry.get_deployment(1, "0x1F98431c8aD98523631AE4a59f267346ea31F984")
    assert dep is not None
    assert dep.pool_init_hash == "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"

    # Pancakeswap V3 mainnet (has custom deployer)
    dep = pool_type_registry.get_deployment(1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865")
    assert dep is not None
    assert dep.deployer is not None
    assert dep.pool_init_hash == "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2"


def test_deployment_data_deployer_falls_back_to_factory() -> None:
    """When no custom deployer, deployment.deployer equals factory address."""
    dep = pool_type_registry.get_deployment(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    assert dep is not None
    assert dep.deployer == "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"


def test_custom_exchange_registration() -> None:
    """A user can register a custom exchange via pool_type_registry.register()."""
    # This replaces the former register_exchange() pattern
    custom_chain = 99999
    custom_factory = get_checksum_address(_generate_random_address())

    pool_type_registry.register(
        UniswapV2Pool,
        chain_id=custom_chain,
        factory_address=custom_factory,
        pool_init_hash="0x0420",
    )
    assert pool_type_registry.has_registration(custom_chain, custom_factory)

    dep = pool_type_registry.get_deployment(custom_chain, custom_factory)
    assert dep is not None
    assert dep.pool_init_hash == "0x0420"

    # Duplicate registration should raise
    with pytest.raises(ValueError):
        pool_type_registry.register(
            UniswapV2Pool,
            chain_id=custom_chain,
            factory_address=custom_factory,
            pool_init_hash="0x0420",
        )
