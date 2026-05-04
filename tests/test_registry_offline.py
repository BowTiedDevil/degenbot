"""
Offline tests for pool and token registries.

Tests that registry methods are correct without requiring a live RPC connection.
"""

from pathlib import Path

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.exceptions import DegenbotValueError
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.registry import managed_pool_registry, pool_registry, token_registry
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

CHAIN_DATA_PATH = Path(__file__).parent / "fixtures" / "chain_data"
UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)


def _get_offline_v2_pool() -> UniswapV2Pool:
    """Construct a V2 pool using offline data."""
    data_file = CHAIN_DATA_PATH / "1" / "block_24945920.json"
    provider = OfflineProvider.from_json_file(data_file)
    adapter = ProviderAdapter.from_offline(provider)
    connection_manager._reset()
    connection_manager.register_provider(adapter)
    connection_manager._default_chain_id = 1
    pool_registry._reset()
    token_registry._reset()
    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        provider=adapter,
        state_block=24945920,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        silent=True,
    )


class FakeUniswapV4Pool(AbstractLiquidityPool):
    """Minimal fake Uniswap V4 pool for testing."""

    def __init__(self, address: str, pool_id: str) -> None:
        self.address = address
        self.pool_id = pool_id
        self.name = f"FakeUniswapV4Pool-{address}"

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)


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
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is lp

    with pytest.raises(DegenbotValueError):
        pool_registry.add(pool_address=lp.address, chain_id=1, pool=lp)


def test_deleting_pool():
    """Removing a pool from the registry makes it unretrievable."""
    pool_registry._reset()
    lp = _get_offline_v2_pool()
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is lp
    pool_registry.remove(pool_address=lp.address, chain_id=1)
    assert pool_registry.get(pool_address=lp.address, chain_id=1) is None


def test_adding_token():
    """Adding a token to the registry makes it retrievable; double-add is an error."""
    pool_registry._reset()
    token_registry._reset()
    lp = _get_offline_v2_pool()
    weth = lp.token1
    assert token_registry.get(token_address=weth.address, chain_id=1) is weth

    with pytest.raises(DegenbotValueError):
        token_registry.add(token_address=weth.address, chain_id=1, token=weth)


def test_deleting_token():
    """Removing a token from the registry makes it unretrievable."""
    pool_registry._reset()
    token_registry._reset()
    lp = _get_offline_v2_pool()
    weth = lp.token1
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
