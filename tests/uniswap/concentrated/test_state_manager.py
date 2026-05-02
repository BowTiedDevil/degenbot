"""Tests for ConcentratedLiquidityStateManager.

The state manager owns the mutable state cache for V3/V4 pools:
- _state_cache deque with a configurable depth limit
- Lock-guarded state reads and writes
- State history management (discard, restore)
- Block-aware push semantics (replace if same block)

It is **pure of I/O**: RPC fetching and event emission stay on the pool.
"""

import dataclasses
from collections import deque

import pytest

from degenbot.uniswap.concentrated.state_manager import ConcentratedLiquidityStateManager


def _make_state(*, block: int, liquidity: int = 1000, sqrt_price_x96: int = 79228162514264337593543950336, tick: int = 0) -> object:
    """Minimal fake state object for testing."""
    return dataclasses.make_dataclass(
        "FakeState",
        [("block", int), ("liquidity", int), ("sqrt_price_x96", int), ("tick", int)],
        frozen=True,
    )(block=block, liquidity=liquidity, sqrt_price_x96=sqrt_price_x96, tick=tick)


class TestStateAccess:
    def test_reads_current_state(self) -> None:
        s0 = _make_state(block=100)
        mgr = ConcentratedLiquidityStateManager(
            initial_state=s0,
            state_cache_depth=8,
        )
        assert mgr.state == s0

    def test_state_is_latest_in_cache(self) -> None:
        s0 = _make_state(block=100)
        s1 = _make_state(block=101)
        mgr = ConcentratedLiquidityStateManager(initial_state=s0, state_cache_depth=4)
        mgr.push_state(s1)
        assert mgr.state == s1

    def test_read_tick_data_proxy(self) -> None:
        """If the state has tick_data, the proxy returns a copy."""
        s0 = dataclasses.make_dataclass(
            "FakeStateWithTicks",
            [("block", int), ("tick_data", dict)],
            frozen=True,
        )(block=100, tick_data={-100: "net_a", 100: "net_b"})

        mgr = ConcentratedLiquidityStateManager(initial_state=s0, state_cache_depth=4)
        copy = mgr.tick_data
        assert copy == s0.tick_data
        assert copy is not s0.tick_data  # must be a copy


class TestPushState:
    def test_appends_different_block(self) -> None:
        s0 = _make_state(block=100)
        s1 = _make_state(block=101)
        mgr = ConcentratedLiquidityStateManager(initial_state=s0, state_cache_depth=4)
        mgr.push_state(s1)
        assert mgr.state == s1
        assert len(mgr._state_cache) == 2

    def test_replaces_same_block(self) -> None:
        s0 = _make_state(block=100, liquidity=1000)
        s1 = _make_state(block=100, liquidity=2000)
        mgr = ConcentratedLiquidityStateManager(initial_state=s0, state_cache_depth=4)
        mgr.push_state(s1)
        assert mgr.state == s1
        assert len(mgr._state_cache) == 1

    def test_respects_cache_depth(self) -> None:
        states = [_make_state(block=n) for n in range(10)]
        mgr = ConcentratedLiquidityStateManager(
            initial_state=states[0],
            state_cache_depth=3,
        )
        for s in states[1:]:
            mgr.push_state(s)
        assert len(mgr._state_cache) == 3
        assert mgr.state.block == 9


class TestDiscardAndRestore:
    def test_discard_older_states(self) -> None:
        states = [_make_state(block=n) for n in [100, 101, 102]]
        mgr = ConcentratedLiquidityStateManager(
            initial_state=states[0],
            state_cache_depth=8,
        )
        for s in states[1:]:
            mgr.push_state(s)

        mgr.discard_states_before_block(101)

        # Should keep 101 and 102
        assert len(mgr._state_cache) == 2
        assert mgr.state.block == 102

    def test_discard_no_op_if_oldest_satisfies(self) -> None:
        s = _make_state(block=200)
        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        mgr.discard_states_before_block(100)
        assert mgr.state.block == 200

    def test_discard_raises_if_all_too_old(self) -> None:
        s = _make_state(block=100)
        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        from degenbot.exceptions.liquidity_pool import NoPoolStateAvailable

        with pytest.raises(NoPoolStateAvailable):
            mgr.discard_states_before_block(101)

    def test_restore_to_last_before_block(self) -> None:
        states = [_make_state(block=n) for n in [100, 101, 102]]
        mgr = ConcentratedLiquidityStateManager(
            initial_state=states[0],
            state_cache_depth=8,
        )
        for s in states[1:]:
            mgr.push_state(s)

        from degenbot.exceptions.liquidity_pool import NoPoolStateAvailable

        restored = mgr.restore_state_before_block(102)
        assert restored.block == 101
        assert mgr.state.block == 101
        assert len(mgr._state_cache) == 2  # dropped 102

    def test_restore_no_op_if_newest_already_before(self) -> None:
        s = _make_state(block=100)
        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        r = mgr.restore_state_before_block(200)
        assert r.block == 100

    def test_restore_raises_if_earliest_too_new(self) -> None:
        s = _make_state(block=100)
        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        from degenbot.exceptions.liquidity_pool import NoPoolStateAvailable

        with pytest.raises(NoPoolStateAvailable):
            mgr.restore_state_before_block(50)


class TestSwapIsViable:
    def test_sparse_always_viable(self) -> None:
        s = _make_state(block=1)
        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        assert mgr.swap_is_viable(state=s, zero_for_one=True, sparse_liquidity_map=True)

    def test_no_tick_data_not_viable(self) -> None:
        s = dataclasses.make_dataclass(
            "FakeState",
            [("block", int), ("sqrt_price_x96", int), ("tick_data", dict)],
            frozen=True,
        )(block=1, sqrt_price_x96=79228162514264337593543950336, tick_data={})

        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        assert not mgr.swap_is_viable(state=s, zero_for_one=True, sparse_liquidity_map=False)

    def test_viable_when_liquidity_beyond_price(self) -> None:
        from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
        import math

        # Create a state with tick data on both sides of price
        tick_data = {-100: object(), 100: object()}
        price = get_sqrt_ratio_at_tick(0)
        s = dataclasses.make_dataclass(
            "FakeState",
            [("block", int), ("sqrt_price_x96", int), ("tick_data", dict)],
            frozen=True,
        )(block=1, sqrt_price_x96=price, tick_data=tick_data)

        mgr = ConcentratedLiquidityStateManager(initial_state=s, state_cache_depth=4)
        assert mgr.swap_is_viable(state=s, zero_for_one=True, sparse_liquidity_map=False)
        assert mgr.swap_is_viable(state=s, zero_for_one=False, sparse_liquidity_map=False)
