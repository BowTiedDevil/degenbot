"""
Tests for pool protocol types.

Verifies that existing pool classes structurally satisfy the defined
protocols once they implement the required methods (simulate_swap,
to_hop_state, extract_fee).
"""

import pytest

from degenbot.types.pool_protocols import (
    ArbitrageCapablePool,
    PoolSimulation,
    ReverseSimulatablePool,
    SimulationResult,
    StateManageablePool,
)


class FakePoolSimulation:
    """Minimal class satisfying PoolSimulation."""

    def __init__(self, address: str = "0x" + "a" * 40) -> None:
        from eth_typing import ChecksumAddress
        from web3 import Web3

        self._address = ChecksumAddress(Web3.to_checksum_address(address))
        self._subscribers: set[object] = set()

    @property
    def address(self):
        return self._address

    def simulate_swap(self, token_in, amount_in, token_out, state_override=None):
        from degenbot.types.abstract import AbstractPoolState

        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_in,
            initial_state=AbstractPoolState(address=self._address, block=1),
            final_state=AbstractPoolState(address=self._address, block=1),
        )

    def subscribe(self, subscriber):
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber):
        self._subscribers.discard(subscriber)


class FakeArbitragePool(FakePoolSimulation):
    """Extends FakePoolSimulation with arbitrage capability."""

    def to_hop_state(self, zero_for_one, state_override=None):
        from fractions import Fraction

        from degenbot.types.hop_types import ConstantProductHop

        return ConstantProductHop(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
        )

    def extract_fee(self, zero_for_one):
        from fractions import Fraction

        return Fraction(3, 1000)


class TestPoolSimulation:
    def test_fake_pool_satisfies_protocol(self):
        pool = FakePoolSimulation()
        assert isinstance(pool, PoolSimulation)

    def test_simulate_swap_returns_result(self):
        pool = FakePoolSimulation()
        result = pool.simulate_swap(
            token_in="0x" + "b" * 40,
            amount_in=1000,
            token_out="0x" + "c" * 40,
        )
        assert isinstance(result, SimulationResult)
        assert result.amount_in == 1000
        assert result.amount_out == 1000

    def test_subscribe_unsubscribe(self):
        pool = FakePoolSimulation()
        subscriber = object()
        pool.subscribe(subscriber)
        assert subscriber in pool._subscribers
        pool.unsubscribe(subscriber)
        assert subscriber not in pool._subscribers


class TestArbitrageCapablePool:
    def test_fake_arbitrage_pool_satisfies_protocol(self):
        pool = FakeArbitragePool()
        assert isinstance(pool, ArbitrageCapablePool)

    def test_to_hop_state_returns_hop_type(self):
        pool = FakeArbitragePool()
        hop = pool.to_hop_state(zero_for_one=True)
        from degenbot.types.hop_types import ConstantProductHop, HopType

        assert isinstance(hop, HopType)
        assert isinstance(hop, ConstantProductHop)

    def test_extract_fee_returns_fraction(self):
        pool = FakeArbitragePool()
        from fractions import Fraction

        fee = pool.extract_fee(zero_for_one=True)
        assert isinstance(fee, Fraction)


class TestReverseSimulatablePool:
    def test_not_satisfied_without_method(self):
        pool = FakePoolSimulation()
        assert not isinstance(pool, ReverseSimulatablePool)

    def test_satisfied_with_method(self):
        class FakeReversePool(FakePoolSimulation):
            def simulate_swap_for_output(
                self, token_in, token_out, amount_out, state_override=None
            ):
                return self.simulate_swap(token_in, amount_out, token_out, state_override)

        pool = FakeReversePool()
        assert isinstance(pool, ReverseSimulatablePool)


class TestStateManageablePool:
    def test_not_satisfied_without_methods(self):
        pool = FakePoolSimulation()
        assert not isinstance(pool, StateManageablePool)

    def test_satisfied_with_methods(self):
        class FakeStateManageablePool(FakePoolSimulation):
            def external_update(self, update):
                pass

            def auto_update(self):
                pass

            def discard_states_before_block(self, block):
                pass

            def restore_state_before_block(self, block):
                pass

        pool = FakeStateManageablePool()
        assert isinstance(pool, StateManageablePool)


class TestSimulationResult:
    def test_frozen(self):
        from degenbot.types.abstract import AbstractPoolState

        result = SimulationResult(
            amount_in=100,
            amount_out=200,
            initial_state=AbstractPoolState(address="0x" + "a" * 40, block=1),
            final_state=AbstractPoolState(address="0x" + "a" * 40, block=2),
        )
        with pytest.raises(AttributeError):
            result.amount_in = 999  # type: ignore[misc]

    def test_fields(self):
        from degenbot.types.abstract import AbstractPoolState

        addr = "0x" + "a" * 40
        result = SimulationResult(
            amount_in=100,
            amount_out=200,
            initial_state=AbstractPoolState(address=addr, block=1),
            final_state=AbstractPoolState(address=addr, block=2),
        )
        assert result.amount_in == 100
        assert result.amount_out == 200
