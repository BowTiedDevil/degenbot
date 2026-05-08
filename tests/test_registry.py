import pytest

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from tests.fakes.pools import FakeUniswapV4Pool

UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def test_distinct_registry_instances():
    pool_registry = PoolRegistry()
    token_registry = TokenRegistry()

    new_pool_registry = PoolRegistry()
    new_token_registry = TokenRegistry()

    assert new_pool_registry is not pool_registry
    assert new_token_registry is not token_registry


def test_adding_pool(bot_mainnet_full: Bot):
    pool = bot_mainnet_full.build_v2_pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert bot_mainnet_full.pools.get(pool_address=pool.address, chain_id=bot_mainnet_full.chain_id) is pool

    with pytest.raises(DegenbotValueError):
        bot_mainnet_full.pools.add(
            pool_address=pool.address, chain_id=bot_mainnet_full.chain_id, pool=pool
        )


def test_deleting_pool(bot_mainnet_full: Bot):
    pool = bot_mainnet_full.build_v2_pool(UNISWAP_V2_WBTC_WETH_POOL)
    assert bot_mainnet_full.pools.get(pool_address=pool.address, chain_id=bot_mainnet_full.chain_id) is pool
    bot_mainnet_full.pools.remove(pool_address=pool.address, chain_id=bot_mainnet_full.chain_id)
    assert (
        bot_mainnet_full.pools.get(pool_address=pool.address, chain_id=bot_mainnet_full.chain_id)
        is None
    )


def test_adding_token(bot_mainnet_full: Bot):
    token = bot_mainnet_full.build_erc20token(WETH_ADDRESS)
    assert (
        bot_mainnet_full.tokens.get(token_address=token.address, chain_id=bot_mainnet_full.chain_id)
        is token
    )

    with pytest.raises(DegenbotValueError):
        bot_mainnet_full.tokens.add(
            token_address=token.address, chain_id=bot_mainnet_full.chain_id, token=token
        )


def test_deleting_token(bot_mainnet_full: Bot):
    token = bot_mainnet_full.build_erc20token(WETH_ADDRESS)
    assert (
        bot_mainnet_full.tokens.get(token_address=token.address, chain_id=bot_mainnet_full.chain_id)
        is token
    )
    bot_mainnet_full.tokens.remove(token_address=token.address, chain_id=bot_mainnet_full.chain_id)
    assert (
        bot_mainnet_full.tokens.get(token_address=token.address, chain_id=bot_mainnet_full.chain_id)
        is None
    )


def test_v4_pool_add_and_removal():
    fake_pool_manager_address = "0x1234567890123456789012345678901234567890"
    fake_pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    # Create mock V4 pool
    fake_pool = FakeUniswapV4Pool(
        address=fake_pool_manager_address,
        pool_id=fake_pool_id,
    )

    # Define V4 pool parameters
    chain_id = 1
    pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    managed_pool_registry = ManagedPoolRegistry()

    # Add the V4 pool to the managed pool registry
    managed_pool_registry.add(
        pool=fake_pool,
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    # Verify the pool was added
    retrieved_pool = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert retrieved_pool is fake_pool, "V4 pool should be added to managed registry"

    # Remove the V4 pool
    managed_pool_registry.remove(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    # Verify the pool is removed from managed pool registry
    pool_after_removal = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert pool_after_removal is None, "V4 pool should be removed from managed registry"

    # Test that removing a non-existent V4 pool doesn't raise an exception
    managed_pool_registry.remove(
        pool_manager_address="0x0000000000000000000000000000000000000000",
        chain_id=chain_id,
        pool_id="0x0000000000000000000000000000000000000000000000000000000000000000",
    )
