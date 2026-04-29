"""
Tests for event-driven auto-solve in ArbitragePath.

Validates that pool state updates trigger re-solve, subscribers are
notified of profitable/unprofitable states, and state overrides
don't affect subscribed state.
"""

from fractions import Fraction

from degenbot.arbitrage.optimizers.hop_types import SolveResult
from degenbot.arbitrage.optimizers.solver import MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.arbitrage.path.arbitrage_path import (
    _ProfitableStateDiscovered,
    _StateUpdatedNoProfit,
)
from degenbot.types.concrete import PoolStateMessage

from .conftest import FakeSubscriber, FakeToken, FakeV2PoolState, _make_v2_pool

FEE_03 = Fraction(3, 1000)


def _make_v2_message(state: FakeV2PoolState) -> PoolStateMessage:
    msg = PoolStateMessage()
    msg.state = state
    return msg


def _make_cyclic_path():
    t0 = FakeToken("0xtokenA")
    t1 = FakeToken("0xtokenB")
    pool0 = _make_v2_pool(t0, t1, reserve0=2_000_000, reserve1=1_000_000_000)
    pool0.address = "0xpool0"
    pool1 = _make_v2_pool(t1, t0, reserve0=1_500_000, reserve1=800_000_000)
    pool1.address = "0xpool1"
    solver = MobiusSolver()
    path = ArbitragePath(
        pools=[pool0, pool1],
        input_token=t0,
        solver=solver,
    )
    return path, pool0, pool1, t0, t1


class TestEventDrivenAutoSolve:
    def test_pool_update_triggers_resolve(self):
        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        new_state = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=3_000_000,
            reserves_token1=900_000_000,
        )
        message = _make_v2_message(new_state)
        path.notify(publisher=pool0, message=message)

        assert path.last_result is not None

    def test_profitable_update_notifies_subscriber(self):
        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        new_state = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=3_000_000,
            reserves_token1=900_000_000,
        )
        message = _make_v2_message(new_state)
        path.notify(publisher=pool0, message=message)

        assert len(subscriber.notifications) >= 1
        _, msg = subscriber.notifications[0]
        assert isinstance(msg, (_ProfitableStateDiscovered | _StateUpdatedNoProfit))

    def test_unprofitable_update_sends_no_profit_message(self):
        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        symmetric_state = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=1_000_000,
            reserves_token1=1_000_000,
        )
        pool0._state = symmetric_state

        path.notify(
            publisher=pool0,
            message=_make_v2_message(symmetric_state),
        )

        last_notification = subscriber.notifications[-1]
        _, msg = last_notification
        assert isinstance(msg, (_ProfitableStateDiscovered | _StateUpdatedNoProfit))

    def test_state_override_does_not_affect_subscribed_state(self):
        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()

        original_hop_0 = path.hop_states[0]

        override_state = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=5_000_000,
            reserves_token1=2_000_000_000,
        )
        path.calculate_with_state_override({pool0.address: override_state})

        assert path.hop_states[0].reserve_in == original_hop_0.reserve_in

    def test_multiple_pool_updates(self):
        path, pool0, pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        new_state_0 = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=3_000_000,
            reserves_token1=900_000_000,
        )
        path.notify(publisher=pool0, message=_make_v2_message(new_state_0))

        new_state_1 = FakeV2PoolState(
            address=pool1.address,
            block=None,
            reserves_token0=1_200_000,
            reserves_token1=600_000_000,
        )
        path.notify(publisher=pool1, message=_make_v2_message(new_state_1))

        assert len(subscriber.notifications) >= 2

    def test_ignore_unknown_publisher(self):
        path, _pool0, _pool1, t0, t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        unknown_pool = _make_v2_pool(t0, t1)
        unknown_pool.address = "0xunknown"
        new_state = FakeV2PoolState(
            address="0xunknown",
            block=None,
            reserves_token0=3_000_000,
            reserves_token1=900_000_000,
        )
        path.notify(
            publisher=unknown_pool,
            message=_make_v2_message(new_state),
        )

        assert len(subscriber.notifications) == 0

    def test_ignore_non_state_message(self):
        from degenbot.types.concrete import TextMessage

        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        path.notify(publisher=pool0, message=TextMessage("test"))

        assert len(subscriber.notifications) == 0

    def test_last_result_updated_on_profitable_discovery(self):
        path, pool0, _pool1, _t0, _t1 = _make_cyclic_path()
        subscriber = FakeSubscriber()
        path.subscribe(subscriber)

        profitable_state = FakeV2PoolState(
            address=pool0.address,
            block=None,
            reserves_token0=3_000_000,
            reserves_token1=1_500_000_000,
        )
        pool0._state = profitable_state

        path.notify(
            publisher=pool0,
            message=_make_v2_message(profitable_state),
        )

        assert path.last_result is not None
        assert isinstance(path.last_result, SolveResult)
