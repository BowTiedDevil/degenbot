"""
Tests for the unified tick data fetcher factory.

Verifies that make_tick_data_fetcher produces a callback that correctly
fetches bitmap/tick data from a concentrated-liquidity pool and pushes
updated state, for both V3 and V4 type variants.
"""

from unittest.mock import MagicMock

import pytest

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.uniswap.concentrated.state_manager import (
    ConcentratedLiquidityStateManager,
)
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
)


def _make_fake_pool(state):
    """
    Build a fake pool with the given state and a real state manager.

    The fake pool exposes tick_bitmap, tick_data, update_block, state,
    get_tick_bitmap_at_word, get_populated_ticks_in_word, and
    _state_mgr.push_state — everything the fetcher needs.
    """
    pool = MagicMock()
    pool.tick_bitmap = dict(state.tick_bitmap)
    pool.tick_data = dict(state.tick_data)
    pool.state = state
    pool.update_block = state.block or 0

    state_mgr = ConcentratedLiquidityStateManager(initial_state=state)
    pool._state_mgr = state_mgr

    return pool


def _make_v3_state(
    tick_bitmap=None, tick_data=None, block=100
) -> UniswapV3PoolState:
    """Build a minimal V3 pool state for testing."""
    return UniswapV3PoolState(
        address="0x" + "00" * 20,
        block=block,
        liquidity=0,
        sqrt_price_x96=2**96,
        tick=0,
        tick_bitmap=tick_bitmap or {},
        tick_data=tick_data or {},
    )


V3_TYPES = TickDataTypes(
    bitmap_at_word=UniswapV3BitmapAtWord,
    liquidity_at_tick=UniswapV3LiquidityAtTick,
)

V4_TYPES = TickDataTypes(
    bitmap_at_word=UniswapV4BitmapAtWord,
    liquidity_at_tick=UniswapV4LiquidityAtTick,
)


class TestNonZeroBitmapUpdatesBitmapAndTickData:
    """
    When the bitmap value is non-zero, the fetcher should:
    1. Update the bitmap at the word position
    2. Fetch and store populated ticks at that word
    3. Push the new state via _state_mgr.push_state()
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_updates_state_on_nonzero_bitmap(self, types):
        state = _make_v3_state(block=100)
        pool = _make_fake_pool(state)

        # Configure the pool to return a non-zero bitmap and one populated tick
        pool.get_tick_bitmap_at_word = MagicMock(return_value=7)
        pool.get_populated_ticks_in_word = MagicMock(
            return_value=[(-10, 500, 100)]
        )

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            provider_lookup=lambda: MagicMock(),  # noqa: PLW0108
            types=types,
        )

        fetcher(word_position=3, block_number=200)

        # The state manager should have the pushed state as current
        new_state = pool._state_mgr.state

        # Bitmap at word 3 should be updated
        assert 3 in new_state.tick_bitmap
        assert new_state.tick_bitmap[3].bitmap == 7
        assert new_state.tick_bitmap[3].block == 200

        # Tick data at tick -10 should be populated
        assert -10 in new_state.tick_data
        assert new_state.tick_data[-10].liquidity_net == 100
        assert new_state.tick_data[-10].liquidity_gross == 500
        assert new_state.tick_data[-10].block == 200

        # Block should be max of original and new
        assert new_state.block == 200


class TestZeroBitmapUpdatesBitmapOnly:
    """
    When the bitmap value is zero, the fetcher should update the bitmap
    but NOT fetch populated ticks.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_updates_bitmap_only_on_zero_bitmap(self, types):
        state = _make_v3_state(block=100)
        pool = _make_fake_pool(state)

        pool.get_tick_bitmap_at_word = MagicMock(return_value=0)
        pool.get_populated_ticks_in_word = MagicMock()

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            provider_lookup=lambda: MagicMock(),  # noqa: PLW0108
            types=types,
        )

        fetcher(word_position=5, block_number=200)

        new_state = pool._state_mgr.state

        # Bitmap updated with value 0
        assert 5 in new_state.tick_bitmap
        assert new_state.tick_bitmap[5].bitmap == 0

        # Populated ticks should NOT have been fetched
        pool.get_populated_ticks_in_word.assert_not_called()

        # No tick data added
        assert len(new_state.tick_data) == 0


class TestPoolNotFound:
    """When pool_lookup returns None, the fetcher should do nothing."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_no_state_change_when_pool_missing(self, types):
        state = _make_v3_state(block=100)
        pool = _make_fake_pool(state)

        # pool_lookup returns None — the pool object is never consulted
        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: None,
            provider_lookup=lambda: MagicMock(),  # noqa: PLW0108
            types=types,
        )

        # Should not raise
        fetcher(word_position=3, block_number=200)

        # State unchanged — still the initial state with no bitmap/tick entries
        new_state = pool._state_mgr.state
        assert len(new_state.tick_bitmap) == 0
        assert len(new_state.tick_data) == 0


class TestBitmapFetchRaises:
    """
    When get_tick_bitmap_at_word raises, the fetcher should return
    early without updating state.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_no_state_change_on_fetch_error(self, types):
        state = _make_v3_state(block=100)
        pool = _make_fake_pool(state)

        pool.get_tick_bitmap_at_word = MagicMock(side_effect=Exception("RPC error"))

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            provider_lookup=lambda: MagicMock(),  # noqa: PLW0108
            types=types,
        )

        # Should not raise
        fetcher(word_position=3, block_number=200)

        # State unchanged from initial
        new_state = pool._state_mgr.state
        assert len(new_state.tick_bitmap) == 0
        assert len(new_state.tick_data) == 0
