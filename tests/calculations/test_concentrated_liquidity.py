"""Tests for apply_liquidity_mapping_update pure function."""

from degenbot.calculations.concentrated_liquidity import apply_liquidity_mapping_update


class TestApplyLiquidityMappingUpdateMint:
    """Minting (positive liquidity) adds ticks and flips the bitmap."""

    def test_mint_creates_new_ticks(self) -> None:
        """A mint in an empty word creates tick entries and flips the bitmap."""
        tick_spacing = 60
        result = apply_liquidity_mapping_update(
            tick_bitmap={},
            tick_data={},
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=0,
            initial_state_block=0,
            update_block=100,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=1000,
        )

        assert -60 in result.tick_data
        assert 60 in result.tick_data
        # Bitmap is keyed by word position, not tick value
        # Tick -60 maps to word -1, tick 60 maps to word 0
        assert -1 in result.tick_bitmap
        assert 0 in result.tick_bitmap

        # liquidity_net: lower tick gets +delta, upper tick gets -delta
        assert result.tick_data[-60].liquidity_net == 1000
        assert result.tick_data[-60].liquidity_gross == 1000
        assert result.tick_data[60].liquidity_net == -1000
        assert result.tick_data[60].liquidity_gross == 1000

    def test_mint_in_existing_tick_accumulates(self) -> None:
        """A second mint in the same range accumulates on existing tick entries."""
        tick_spacing = 60

        # First mint
        result = apply_liquidity_mapping_update(
            tick_bitmap={},
            tick_data={},
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=0,
            initial_state_block=0,
            update_block=100,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=1000,
        )

        # Second mint in the same range
        result = apply_liquidity_mapping_update(
            tick_bitmap=result.tick_bitmap,
            tick_data=result.tick_data,
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=result.liquidity,
            initial_state_block=0,
            update_block=101,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=500,
        )

        assert result.tick_data[-60].liquidity_net == 1500
        assert result.tick_data[-60].liquidity_gross == 1500
        assert result.tick_data[60].liquidity_net == -1500
        assert result.tick_data[60].liquidity_gross == 1500

    def test_mint_in_range_adjusts_liquidity(self) -> None:
        """Minting in a range that includes the current tick adjusts in-range liquidity."""
        tick_spacing = 60

        result = apply_liquidity_mapping_update(
            tick_bitmap={},
            tick_data={},
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=0,
            initial_state_block=0,  # block 0 < update_block 100, so in-range check runs
            update_block=100,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=1000,
        )

        # Current tick (0) is in [-60, 60), so liquidity should be adjusted
        assert result.liquidity == 1000


class TestApplyLiquidityMappingUpdateBurn:
    """Burning (negative liquidity) removes or decrements ticks."""

    def test_burn_removes_tick_with_zero_gross(self) -> None:
        """A burn that zeroes gross liquidity removes the tick and flips the bitmap."""
        tick_spacing = 60

        # First mint
        result = apply_liquidity_mapping_update(
            tick_bitmap={},
            tick_data={},
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=0,
            initial_state_block=0,
            update_block=100,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=1000,
        )

        # Now burn the same amount
        result = apply_liquidity_mapping_update(
            tick_bitmap=result.tick_bitmap,
            tick_data=result.tick_data,
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=result.liquidity,
            initial_state_block=0,
            update_block=101,
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=-1000,
        )

        # Ticks should be removed (gross went to 0)
        assert -60 not in result.tick_data
        assert 60 not in result.tick_data
        # Liquidity returns to 0
        assert result.liquidity == 0


class TestApplyLiquidityMappingUpdateSkipInRange:
    """In-range liquidity adjustment is skipped when update_block <= initial_state_block."""

    def test_skip_in_range_when_block_not_past_initial(self) -> None:
        """
        When update_block <= initial_state_block, in-range liquidity is NOT adjusted.
        This matches the mock pool's behavior with initial_state_block=MAX_UINT256.
        """
        tick_spacing = 60

        result = apply_liquidity_mapping_update(
            tick_bitmap={},
            tick_data={},
            tick_spacing=tick_spacing,
            tick=0,
            liquidity=0,
            initial_state_block=2**256 - 1,  # MAX_UINT256 — skip in-range check
            update_block=100,  # less than initial_state_block
            tick_lower=-60,
            tick_upper=60,
            liquidity_delta=1000,
        )

        # Liquidity should NOT be adjusted even though tick 0 is in range
        assert result.liquidity == 0
        # But tick entries should still be created
        assert -60 in result.tick_data
        assert 60 in result.tick_data
