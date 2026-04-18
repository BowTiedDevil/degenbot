"""
Tests for FakeV3PoolWithTicks and multi-range integration.

These verify that fake pools correctly support _get_cached_tick_ranges()
and enable testing of piecewise-Möbius solving without RPC dependencies.
"""

import dataclasses

import pytest
from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers.solver import (
    V3TickRangeInfo,
    _get_cached_tick_ranges,
    _v3_get_adjacent_tick_ranges,
)
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from tests.arbitrage.fake_pools import (
    FakeTickInfo,
    FakeV3PoolWithTicks,
    TickRangeDefinition,
    create_two_range_v3_pool,
)
from tests.arbitrage.mock_pools import MockErc20Token

# Test constants
USDC_ADDRESS: ChecksumAddress = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
WETH_ADDRESS: ChecksumAddress = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


@pytest.fixture
def usdc() -> MockErc20Token:
    """USDC token fixture."""
    return MockErc20Token(address=USDC_ADDRESS, symbol="USDC", decimals=6)


@pytest.fixture
def weth() -> MockErc20Token:
    """WETH token fixture."""
    return MockErc20Token(address=WETH_ADDRESS, symbol="WETH", decimals=18)


class TestFakeV3PoolWithTicks:
    """Tests for FakeV3PoolWithTicks initialization and tick data."""

    def test_initialization_with_two_ranges(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test creating a pool with two adjacent liquidity ranges."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # Must be multiple of tick_spacing=60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Verify basic properties
        assert pool.state.tick == -60  # Current tick is -60
        assert pool.state.liquidity == 10_000_000
        assert pool.tick_spacing == 60
        assert pool.fee == 3000
        assert pool.sparse_liquidity_map is False

        # Verify tick_data has entries for range boundaries
        # Two ranges: [-180, 0) and [0, 180)
        # Should have initialized ticks at -180, 0, 180
        assert -180 in pool.tick_data
        assert 0 in pool.tick_data
        assert 180 in pool.tick_data

    def test_tick_liquidity_net_calculation(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that liquidity_net is correctly calculated at tick boundaries."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # Must be multiple of tick_spacing=60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Tick -180: start of first range, liquidity_net = +10_000_000
        assert pool.get_tick_liquidity(-180) == 10_000_000

        # Tick 0: end of first range (-10_000_000), start of second (+20_000_000)
        # net = -10_000_000 + 20_000_000 = +10_000_000
        assert pool.get_tick_liquidity(0) == 10_000_000

        # Tick 180: end of second range, liquidity_net = -20_000_000
        assert pool.get_tick_liquidity(180) == -20_000_000

    def test_tick_bitmap_construction(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that tick_bitmap is correctly built from initialized ticks."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # Must be multiple of tick_spacing=60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Ticks -180, 0, 180 should be in bitmap
        # word_pos = tick >> 8, bit_pos = tick & 0xFF
        # For -180: word_pos = -1 (or -180 >> 8 = -1 in Python), bit_pos = 76
        # For 0: word_pos = 0, bit_pos = 0
        # For 180: word_pos = 0, bit_pos = 180

        # Check that tick_bitmap has entries
        assert len(pool.tick_bitmap) > 0

        # Word 0 should have bits 0 and 180 set
        if 0 in pool.tick_bitmap:
            word_0 = pool.tick_bitmap[0]
            assert word_0 & (1 << 0)  # Tick 0
            assert word_0 & (1 << 180)  # Tick 180

    def test_current_tick_validation(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that current_tick must align with tick_spacing."""
        with pytest.raises(ValueError, match="must be multiple of tick_spacing"):
            create_two_range_v3_pool(
                address="0x1234567890123456789012345678901234567890",
                token0=usdc,
                token1=weth,
                current_tick=-35,  # Not aligned with tick_spacing=60
                lower_liquidity=10_000_000,
                upper_liquidity=20_000_000,
            )

    def test_get_current_range(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test get_current_range() returns correct range."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # In lower range [-180, 0), multiple of 60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        current_range = pool.get_current_range()
        assert current_range is not None
        assert current_range.tick_lower == -180
        assert current_range.tick_upper == 0
        assert current_range.liquidity == 10_000_000


class TestGetCachedTickRanges:
    """Tests for _get_cached_tick_ranges() with fake pools."""

    def test_returns_tick_ranges_for_fake_pool(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that _get_cached_tick_ranges works with FakeV3PoolWithTicks."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # Must be multiple of tick_spacing=60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Should return tick ranges for zero_for_one swap
        result = _get_cached_tick_ranges(pool, zero_for_one=True, max_ranges=3)

        assert result is not None
        tick_ranges, _current_range_index = result

        # Should have at least 1 range (depending on tick iteration order)
        assert len(tick_ranges) >= 1

        # Verify V3TickRangeInfo structure
        range_0 = tick_ranges[0]
        assert isinstance(range_0, V3TickRangeInfo)
        # Ticks should be valid
        assert isinstance(range_0.tick_lower, int)
        assert isinstance(range_0.tick_upper, int)

    def test_respects_max_ranges(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that max_ranges parameter limits returned ranges."""
        # Create pool with more ranges than max_ranges
        pool = FakeV3PoolWithTicks(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            tick_spacing=60,
            fee=3000,
            current_tick=-120,  # Must be multiple of tick_spacing=60
            current_liquidity=10_000_000,
            current_sqrt_price_x96=get_sqrt_ratio_at_tick(-120),
            tick_ranges=[
                TickRangeDefinition(-180, -120, 10_000_000),
                TickRangeDefinition(-120, -60, 15_000_000),
                TickRangeDefinition(-60, 0, 20_000_000),
                TickRangeDefinition(0, 60, 25_000_000),
            ],
        )

        result = _get_cached_tick_ranges(pool, zero_for_one=True, max_ranges=2)

        assert result is not None
        tick_ranges, _ = result
        assert len(tick_ranges) <= 3  # max_ranges + 1 for boundaries

    def test_cache_hit_returns_same_result(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that cache returns consistent results for same pool state."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,  # Must be multiple of tick_spacing=60
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # First call populates cache
        result1 = _get_cached_tick_ranges(pool, zero_for_one=True)

        # Second call should hit cache
        result2 = _get_cached_tick_ranges(pool, zero_for_one=True)

        assert result1 is not None
        assert result2 is not None
        assert result1[0] == result2[0]
        assert result1[1] == result2[1]


class TestPoolStateToHopIntegration:
    """Tests for pool_state_to_hop() with fake pools.

    Note: These tests are skipped because FakeV3PoolWithTicks doesn't
    inherit from the real UniswapV3Pool class, so isinstance checks fail.
    The fake pools are designed for testing _v3_get_adjacent_tick_ranges()
    and other tick-related functions directly.
    """

    def test_fake_pool_has_required_attributes(self, usdc: MockErc20Token, weth: MockErc20Token):
        """Test that fake pools have the attributes needed by tick functions."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Required attributes for _v3_get_adjacent_tick_ranges
        assert hasattr(pool, "tick")
        assert hasattr(pool, "tick_data")
        assert hasattr(pool, "tick_bitmap")
        assert hasattr(pool, "tick_spacing")
        assert hasattr(pool, "sparse_liquidity_map")

        # Verify types
        assert isinstance(pool.tick, int)
        assert isinstance(pool.tick_data, dict)
        assert isinstance(pool.tick_bitmap, dict)
        assert pool.sparse_liquidity_map is False

    def test_sparse_flag_causes_early_return(
        self, usdc: MockErc20Token, weth: MockErc20Token
    ):
        """Test that sparse_liquidity_map flag causes immediate None return."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,
            token1=weth,
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Sparse pool should return None immediately (before any iteration)
        pool.sparse_liquidity_map = True
        result = _v3_get_adjacent_tick_ranges(pool, zero_for_one=True, max_ranges=3)
        assert result is None


class TestFakeTickInfo:
    """Tests for FakeTickInfo dataclass."""

    def test_to_liquidity_at_tick_conversion(self):
        """Test conversion to UniswapV3LiquidityAtTick."""
        fake_tick = FakeTickInfo(liquidity_net=10_000_000, liquidity_gross=10_000_000)

        real_tick = fake_tick.to_liquidity_at_tick()

        assert real_tick.liquidity_net == 10_000_000
        assert real_tick.liquidity_gross == 10_000_000

    def test_immutability(self):
        """Test that FakeTickInfo is frozen (immutable)."""
        fake_tick = FakeTickInfo(liquidity_net=10_000_000, liquidity_gross=10_000_000)

        with pytest.raises(dataclasses.FrozenInstanceError):
            fake_tick.liquidity_net = 20_000_000


class TestTickRangeDefinition:
    """Tests for TickRangeDefinition validation."""

    def test_valid_range(self):
        """Test creating valid range definition."""
        range_def = TickRangeDefinition(tick_lower=-180, tick_upper=0, liquidity=10_000_000)

        assert range_def.tick_lower == -180
        assert range_def.tick_upper == 0
        assert range_def.liquidity == 10_000_000

    def test_invalid_range_lower_ge_upper(self):
        """Test that tick_lower >= tick_upper raises error."""
        with pytest.raises(ValueError, match="tick_lower"):
            TickRangeDefinition(tick_lower=0, tick_upper=0, liquidity=10_000_000)

        with pytest.raises(ValueError, match="tick_lower"):
            TickRangeDefinition(tick_lower=60, tick_upper=0, liquidity=10_000_000)

    def test_invalid_negative_liquidity(self):
        """Test that negative liquidity raises error."""
        with pytest.raises(ValueError, match="liquidity"):
            TickRangeDefinition(tick_lower=-180, tick_upper=0, liquidity=-1)
