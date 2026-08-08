from pathlib import Path

import pytest

from degenbot.bot import Bot, PyBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import OfflineProvider
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.fakes.pools import FakeUniswapV4Pool
from tests.golden.recorded_pool import load_pool
from tests.helpers.erc20_factory import make_erc20

UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

_V2_WBTC_WETH_GOLDEN = Path("tests/golden/data/uniswap/v2/wbtc_weth/17600000.json")
_PY_BOT = PyBot()


def _offline_bot() -> Bot:
    """A single-chain Bot with a no-op offline provider (registry tests need no network)."""
    config = DegenbotConfig(
        database=DatabaseSettings(path=Path(":memory:")),
        rpc={},
        default_chain_id=1,
    )
    provider = OfflineProvider(chain_id=1, blocks={"1": {"timestamp": 0, "calls": {}, "code": {}}})
    return Bot(config, provider=provider)


def _recorded_pool() -> UniswapV2Pool:
    """A real I/O-free WBTC/WETH V2 pool rebuilt from the recorded golden (no RPC)."""
    return load_pool(_V2_WBTC_WETH_GOLDEN, chain_id=1, block=17_600_000)


def _weth_token() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )


def test_distinct_registry_instances():
    pool_registry = PoolRegistry()
    token_registry = TokenRegistry()

    new_pool_registry = PoolRegistry()
    new_token_registry = TokenRegistry()

    assert new_pool_registry is not pool_registry
    assert new_token_registry is not token_registry


def test_adding_pool():
    # Registry semantics are network-independent; seed a real io-free pool object.
    bot = _offline_bot()
    pool = _recorded_pool()
    bot.pools.add(pool_address=pool.address, chain_id=bot.chain_id, pool=pool)
    assert bot.pools.get(pool_address=pool.address, chain_id=bot.chain_id) is pool

    with pytest.raises(DegenbotValueError):
        bot.pools.add(pool_address=pool.address, chain_id=bot.chain_id, pool=pool)


def test_deleting_pool():
    bot = _offline_bot()
    pool = _recorded_pool()
    bot.pools.add(pool_address=pool.address, chain_id=bot.chain_id, pool=pool)
    assert bot.pools.get(pool_address=pool.address, chain_id=bot.chain_id) is pool
    bot.pools.remove(pool_address=pool.address, chain_id=bot.chain_id)
    assert bot.pools.get(pool_address=pool.address, chain_id=bot.chain_id) is None


def test_adding_token():
    bot = _offline_bot()
    token = _weth_token()
    bot.tokens.add(token_address=token.address, chain_id=bot.chain_id, token=token)
    assert bot.tokens.get(token_address=token.address, chain_id=bot.chain_id) is token

    with pytest.raises(DegenbotValueError):
        bot.tokens.add(token_address=token.address, chain_id=bot.chain_id, token=token)


def test_deleting_token():
    bot = _offline_bot()
    token = _weth_token()
    bot.tokens.add(token_address=token.address, chain_id=bot.chain_id, token=token)
    assert bot.tokens.get(token_address=token.address, chain_id=bot.chain_id) is token
    bot.tokens.remove(token_address=token.address, chain_id=bot.chain_id)
    assert bot.tokens.get(token_address=token.address, chain_id=bot.chain_id) is None


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
