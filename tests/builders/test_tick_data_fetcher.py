"""
Tests for the unified tick data fetcher factory.

Verifies that make_tick_data_fetcher produces a callback that correctly
fetches bitmap/tick data from a concentrated-liquidity pool and pushes
updated state, for both V3 and V4 type variants.
"""

from unittest.mock import MagicMock, patch

import eth_abi.abi
import pytest

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.uniswap.concentrated.state_manager import (
    ConcentratedLiquidityStateManager,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
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
    address, tick_spacing, and _state_mgr.push_state — everything the
    fetcher needs.
    """
    pool = MagicMock()
    pool.address = "0x" + "00" * 20
    pool.tick_bitmap = dict(state.tick_bitmap)
    pool.tick_data = dict(state.tick_data)
    pool.state = state
    pool.update_block = state.block or 0
    pool.tick_spacing = 60

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
    tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
)

V4_TYPES = TickDataTypes(
    bitmap_at_word=UniswapV4BitmapAtWord,
    liquidity_at_tick=UniswapV4LiquidityAtTick,
    tick_struct_types=("uint128", "int128"),
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

        # Configure raw_call to return a non-zero bitmap (7 = bits 0,1,2 set)
        # This means ticks at positions (3<<8 + 0)*60, (3<<8 + 1)*60, (3<<8 + 2)*60
        # = 46080, 46140, 46200
        bitmap_value = 7

        # Configure provider.call to return tick data for the 3 active ticks
        tick_data_responses = {}
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            # Encode using the full tick struct types
            # V3: (uint128, int128, uint256, uint256, int56, uint160, uint32, bool)
            # V4: (uint128, int128)
            if len(types.tick_struct_types) == 8:
                tick_data_responses[tick] = eth_abi.abi.encode(
                    types=types.tick_struct_types,
                    args=[500, 100, 0, 0, 0, 0, 0, False],
                )
            else:
                tick_data_responses[tick] = eth_abi.abi.encode(
                    types=types.tick_struct_types,
                    args=[500, 100],
                )

        def mock_call(*, to, data, block=None):
            # Check if it's a ticks(int24) call by looking for the selector
            # We'll match by checking if any of our known ticks are being queried
            for tick, response in tick_data_responses.items():
                tick_calldata = _encode_ticks_calldata(tick)
                if data == tick_calldata:
                    return response
            msg = f"Unexpected call with data={data!r}"
            raise ValueError(msg)

        provider = MagicMock()
        provider.call = MagicMock(side_effect=mock_call)

        with patch("degenbot.builders.tick_data_fetcher.raw_call") as mock_raw_call:
            mock_raw_call.return_value = (bitmap_value,)

            fetcher = make_tick_data_fetcher(
                pool_lookup=lambda _: pool,
                provider_lookup=lambda: provider,
                types=types,
            )

            fetcher(word_position=3, block_number=200)

        # The state manager should have the pushed state as current
        new_state = pool._state_mgr.state

        # Bitmap at word 3 should be updated
        assert 3 in new_state.tick_bitmap
        assert new_state.tick_bitmap[3].bitmap == 7
        assert new_state.tick_bitmap[3].block == 200

        # Tick data should be populated for all 3 active ticks
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            assert tick in new_state.tick_data
            assert new_state.tick_data[tick].liquidity_net == 100
            assert new_state.tick_data[tick].liquidity_gross == 500
            assert new_state.tick_data[tick].block == 200

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

        provider = MagicMock()
        provider.call = MagicMock()  # Should NOT be called

        with patch("degenbot.builders.tick_data_fetcher.raw_call") as mock_raw_call:
            mock_raw_call.return_value = (0,)

            fetcher = make_tick_data_fetcher(
                pool_lookup=lambda _: pool,
                provider_lookup=lambda: provider,
                types=types,
            )

            fetcher(word_position=5, block_number=200)

        new_state = pool._state_mgr.state

        # Bitmap updated with value 0
        assert 5 in new_state.tick_bitmap
        assert new_state.tick_bitmap[5].bitmap == 0

        # Provider.call should NOT have been called (no tick fetching)
        provider.call.assert_not_called()

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
    When the bitmap RPC call raises, the fetcher should return
    early without updating state.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_no_state_change_on_fetch_error(self, types):
        state = _make_v3_state(block=100)
        pool = _make_fake_pool(state)

        with patch("degenbot.builders.tick_data_fetcher.raw_call") as mock_raw_call:
            mock_raw_call.side_effect = Exception("RPC error")

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


# --- Helper for encoding ticks(int24) calldata ---


def _encode_ticks_calldata(tick: int) -> bytes:
    """Encode a ticks(int24) call for the given tick value."""
    from degenbot.provider.call_helpers import encode_function_calldata

    return encode_function_calldata("ticks(int24)", [tick])
