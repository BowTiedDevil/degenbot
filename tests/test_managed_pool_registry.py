"""Tests for the ManagedPoolRegistry (V4 pools keyed by pool manager + pool ID)."""

import pytest
from hexbytes import HexBytes

from degenbot.exceptions import DegenbotValueError
from degenbot.registry.pool import ManagedPoolRegistry
from degenbot.types.abstract import AbstractLiquidityPool


class FakeUniswapV4Pool(AbstractLiquidityPool):
    """Minimal fake V4 pool for registry tests."""

    def __init__(self, address: str, pool_id: str) -> None:
        self.address = address
        self.pool_id = pool_id
        self.name = f"FakeV4Pool-{address[:8]}"

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)


FAKE_POOL_MANAGER = "0x1234567890123456789012345678901234567890"
FAKE_POOL_ID = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"


def test_get_missing_returns_none():
    registry = ManagedPoolRegistry()
    assert (
        registry.get(
            chain_id=1,
            pool_manager_address=FAKE_POOL_MANAGER,
            pool_id=FAKE_POOL_ID,
        )
        is None
    )


def test_add_and_retrieve_pool():
    registry = ManagedPoolRegistry()
    pool = FakeUniswapV4Pool(FAKE_POOL_MANAGER, FAKE_POOL_ID)

    registry.add(
        pool=pool,
        chain_id=1,
        pool_manager_address=FAKE_POOL_MANAGER,
        pool_id=FAKE_POOL_ID,
    )

    assert (
        registry.get(
            chain_id=1,
            pool_manager_address=FAKE_POOL_MANAGER,
            pool_id=FAKE_POOL_ID,
        )
        is pool
    )


def test_add_duplicate_raises():
    registry = ManagedPoolRegistry()
    pool = FakeUniswapV4Pool(FAKE_POOL_MANAGER, FAKE_POOL_ID)

    registry.add(
        pool=pool,
        chain_id=1,
        pool_manager_address=FAKE_POOL_MANAGER,
        pool_id=FAKE_POOL_ID,
    )

    with pytest.raises(DegenbotValueError):
        registry.add(
            pool=pool,
            chain_id=1,
            pool_manager_address=FAKE_POOL_MANAGER,
            pool_id=FAKE_POOL_ID,
        )


def test_remove_pool():
    registry = ManagedPoolRegistry()
    pool = FakeUniswapV4Pool(FAKE_POOL_MANAGER, FAKE_POOL_ID)

    registry.add(
        pool=pool,
        chain_id=1,
        pool_manager_address=FAKE_POOL_MANAGER,
        pool_id=FAKE_POOL_ID,
    )
    registry.remove(
        chain_id=1,
        pool_manager_address=FAKE_POOL_MANAGER,
        pool_id=FAKE_POOL_ID,
    )
    assert (
        registry.get(
            chain_id=1,
            pool_manager_address=FAKE_POOL_MANAGER,
            pool_id=FAKE_POOL_ID,
        )
        is None
    )


def test_remove_missing_is_noop():
    registry = ManagedPoolRegistry()
    registry.remove(
        chain_id=1,
        pool_manager_address=FAKE_POOL_MANAGER,
        pool_id=FAKE_POOL_ID,
    )
    assert (
        registry.get(
            chain_id=1,
            pool_manager_address=FAKE_POOL_MANAGER,
            pool_id=FAKE_POOL_ID,
        )
        is None
    )
