"""Tests for pool protocol types.

Verifies that existing pool classes structurally satisfy the defined
protocols once they implement the required methods (subscribe, unsubscribe).
"""

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.pool_protocols import (
    PoolSimulation,
    StateManageablePool,
)


class FakePoolSimulation:
    """Minimal class satisfying PoolSimulation."""

    def __init__(self, address: str = "0x" + "a" * 40) -> None:

        self._address = ChecksumAddress(get_checksum_address(address))
        self._subscribers: set[object] = set()

    @property
    def address(self):
        return self._address

    def subscribe(self, subscriber):
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber):
        self._subscribers.discard(subscriber)


class TestPoolSimulation:
    def test_fake_pool_satisfies_protocol(self):
        pool = FakePoolSimulation()
        assert isinstance(pool, PoolSimulation)

    def test_subscribe_unsubscribe(self):
        pool = FakePoolSimulation()
        subscriber = object()
        pool.subscribe(subscriber)
        assert subscriber in pool._subscribers
        pool.unsubscribe(subscriber)
        assert subscriber not in pool._subscribers


class TestStateManageablePool:
    def test_not_satisfied_without_methods(self):
        pool = FakePoolSimulation()
        assert not isinstance(pool, StateManageablePool)

    def test_satisfied_with_methods(self):
        class FakeStateManageablePool(FakePoolSimulation):
            def external_update(self, update):
                pass

            def discard_states_before_block(self, block):
                pass

            def restore_state_before_block(self, block):
                pass

        pool = FakeStateManageablePool()
        assert isinstance(pool, StateManageablePool)
