"""
Offline tests for pool and token registries.

Tests that registry methods are correct without requiring a live RPC connection.
"""

from pathlib import Path

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.registry import managed_pool_registry, pool_registry, token_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.fakes.pools import FakeUniswapV4Pool

CHAIN_DATA_PATH = Path(__file__).parent / "fixtures" / "chain_data"
UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)


def _get_offline_v2_pool() -> UniswapV2Pool:
    """Construct a V2 pool using offline data."""
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.functions import encode_function_calldata, raw_call

    data_file = CHAIN_DATA_PATH / "1" / "block_24945920.json"
    provider = OfflineProvider.from_json_file(data_file)
    adapter = ProviderAdapter.from_offline(provider)

    factory_address = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    wbtc = Erc20Token(
        get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )
    weth = Erc20Token(
        get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )
    (reserves0, reserves1, *_) = raw_call(
        adapter,
        address=UNISWAP_V2_WBTC_WETH_POOL,
        calldata=encode_function_calldata(
            function_prototype="getReserves()",
            function_arguments=None,
        ),
        return_types=["uint112", "uint112", "uint32"],
        block_identifier=24945920,
    )
    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        state_block=24945920,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        token0=wbtc,
        token1=weth,
        factory=factory_address,
        fee_token0=UniswapV2Pool.FEE,
        fee_token1=UniswapV2Pool.FEE,
        reserves_token0=reserves0,
        reserves_token1=reserves1,
    )


def test_singleton():
    """
    Constructing a new registry instance returns a distinct object.
    (The global singletons are checked, not the class-level one.)
    """
    new_pool_registry = type(pool_registry)()
    new_token_registry = type(token_registry)()

    assert new_pool_registry is not pool_registry
    assert new_token_registry is not token_registry


def test_adding_pool():
    """Adding a pool to the registry makes it retrievable; double-add is an error."""
    pool_registry._reset()
    lp = _get_offline_v2_pool()
    pool_registry.add(pool_address=lp.address, chain_id=1, pool=lp)
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is lp

    with pytest.raises(DegenbotValueError):
        pool_registry.add(pool_address=lp.address, chain_id=1, pool=lp)


def test_deleting_pool():
    """Removing a pool from the registry makes it unretrievable."""
    pool_registry._reset()
    lp = _get_offline_v2_pool()
    pool_registry.add(pool_address=lp.address, chain_id=1, pool=lp)
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is lp
    pool_registry.remove(pool_address=lp.address, chain_id=1)
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is None


def test_adding_token():
    """Adding a token to the registry makes it retrievable; double-add is an error."""
    pool_registry._reset()
    token_registry._reset()
    lp = _get_offline_v2_pool()
    weth = lp.token1
    token_registry.add(token_address=weth.address, chain_id=1, token=weth)
    assert token_registry.get(token_address=weth.address, chain_id=1) is weth

    with pytest.raises(DegenbotValueError):
        token_registry.add(token_address=weth.address, chain_id=1, token=weth)


def test_deleting_token():
    """Removing a token from the registry makes it unretrievable."""
    pool_registry._reset()
    token_registry._reset()
    lp = _get_offline_v2_pool()
    weth = lp.token1
    token_registry.add(token_address=weth.address, chain_id=1, token=weth)
    assert token_registry.get(token_address=weth.address, chain_id=1) is weth
    token_registry.remove(token_address=weth.address, chain_id=1)
    assert token_registry.get(token_address=weth.address, chain_id=1) is None


def test_v4_pool_add_and_removal():
    """Managed pool registry supports V4-style pools with pool_manager_address + pool_id."""
    fake_pool_manager_address = "0x1234567890123456789012345678901234567890"
    fake_pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    fake_pool = FakeUniswapV4Pool(
        address=fake_pool_manager_address,
        pool_id=fake_pool_id,
    )

    chain_id = 1
    pool_id = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"

    managed_pool_registry.add(
        pool=fake_pool,
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    retrieved_pool = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert retrieved_pool is fake_pool

    managed_pool_registry.remove(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )

    pool_after_removal = managed_pool_registry.get(
        chain_id=chain_id,
        pool_manager_address=fake_pool_manager_address,
        pool_id=pool_id,
    )
    assert pool_after_removal is None

    # Removing a non-existent pool must not raise
    managed_pool_registry.remove(
        pool_manager_address="0x0000000000000000000000000000000000000000",
        chain_id=chain_id,
        pool_id="0x0000000000000000000000000000000000000000000000000000000000000000",
    )
