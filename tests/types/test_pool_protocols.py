"""Tests for pool protocol types.

Verifies that existing pool classes structurally satisfy the defined
protocols once they implement the required methods.
"""

from __future__ import annotations

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.pool_protocols import (
    PoolSimulation,
    StateManageablePool,
)


class FakePoolSimulation:
    """Minimal class satisfying PoolSimulation."""

    def __init__(self, address: str = "0x" + "a" * 40):
        self._address = get_checksum_address(address)

    @property
    def address(self):
        return self._address


class TestPoolSimulation:
    def test_fake_pool_satisfies_protocol(self):
        pool = FakePoolSimulation()
        assert isinstance(pool, PoolSimulation)


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
