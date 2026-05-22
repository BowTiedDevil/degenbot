"""
Tests for production UniswapV3Pool with tick data for multi-range integration.

These verify that production V3 pools correctly support _get_cached_tick_ranges()
and enable testing of piecewise-Möbius solving without RPC dependencies.

Production pools are fully I/O-free when constructed directly.
"""

import dataclasses

import pytest
from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers._v3_utils import (
    _get_cached_tick_ranges,
    _v3_get_adjacent_tick_ranges,
)
from degenbot.types.hop_types import V3TickRangeInfo
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.arbitrage.fake_pools import (
    TickRangeDefinition,
    build_v3_pool_with_ticks,
    create_two_range_v3_pool,
)
from tests.fakes.tokens import FakeToken

# Test constants
USDC_ADDRESS: ChecksumAddress = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
WETH_ADDRESS: ChecksumAddress = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


@pytest.fixture
def usdc() -> FakeToken:
    """USDC token fixture."""
    return FakeToken(address=USDC_ADDRESS, symbol="USDC", decimals=6)


@pytest.fixture
def weth() -> FakeToken:
    """WETH token fixture."""
    return FakeToken(address=WETH_ADDRESS, symbol="WETH", decimals=18)


class TestV3PoolWithTicks:
    """Tests for production UniswapV3Pool with tick data from fake_pools builders."""

    def test_initialization_with_two_ranges(self, usdc: FakeToken, weth: FakeToken):
        """Test creating a pool with two adjacent liquidity ranges."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Verify basic properties
        assert isinstance(pool, UniswapV3Pool)
        assert pool.state.tick == -60
        assert pool.state.liquidity == 10_000_000
        assert pool.tick_spacing == 60
        assert pool.fee == 3000
        # Production pool with tick data should NOT be sparse
        assert pool.sparse_liquidity_map is False

        # Verify tick_data has entries for range boundaries
        # Two ranges: [-180, 0) and [0, 180)
        # Should have initialized ticks at -180, 0, 180
        assert -180 in pool.state.tick_data
        assert 0 in pool.state.tick_data
        assert 180 in pool.state.tick_data

    def test_tick_liquidity_net_calculation(self, usdc: FakeToken, weth: FakeToken):
        """Test that liquidity_net is correctly calculated at tick boundaries."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        tick_data = pool.state.tick_data

        # Tick -180: start of first range, liquidity_net = +10_000_000
        assert tick_data[-180].liquidity_net == 10_000_000

        # Tick 0: end of first range (-10_000_000), start of second (+20_000_000)
        # net = -10_000_000 + 20_000_000 = +10_000_000
        assert tick_data[0].liquidity_net == 10_000_000

        # Tick 180: end of second range, liquidity_net = -20_000_000
        assert tick_data[180].liquidity_net == -20_000_000

    def test_tick_bitmap_construction(self, usdc: FakeToken, weth: FakeToken):
        """Test that tick_bitmap is correctly built from initialized ticks."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # tick_bitmap should have entries
        assert len(pool.state.tick_bitmap) > 0

        # Word 0 should have bits 0 and 180 set (for ticks 0 and 180)
        if 0 in pool.state.tick_bitmap:
            bitmap_0 = pool.state.tick_bitmap[0].bitmap
            assert bitmap_0 & (1 << 0)  # Tick 0
            assert bitmap_0 & (1 << 180)  # Tick 180

    def test_current_tick_validation(self, usdc: FakeToken, weth: FakeToken):
        """Test that current_tick must align with tick_spacing."""
        with pytest.raises(ValueError, match="must be multiple of tick_spacing"):
            create_two_range_v3_pool(
                address="0x1234567890123456789012345678901234567890",
                token0=usdc,  # type: ignore[arg-type]
                token1=weth,  # type: ignore[arg-type]
                current_tick=-35,  # Not aligned with tick_spacing=60
                lower_liquidity=10_000_000,
                upper_liquidity=20_000_000,
            )


class TestGetCachedTickRanges:
    """Tests for _get_cached_tick_ranges() with production V3 pools."""

    def test_returns_tick_ranges_for_pool(self, usdc: FakeToken, weth: FakeToken):
        """Test that _get_cached_tick_ranges works with production UniswapV3Pool."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=True,
            max_ranges=3,
        )

        assert result is not None
        tick_ranges, _current_range_index = result

        # Should have at least 1 range
        assert len(tick_ranges) >= 1

        # Verify V3TickRangeInfo structure
        range_0 = tick_ranges[0]
        assert isinstance(range_0, V3TickRangeInfo)
        assert isinstance(range_0.tick_lower, int)
        assert isinstance(range_0.tick_upper, int)

    def test_respects_max_ranges(self, usdc: FakeToken, weth: FakeToken):
        """Test that max_ranges parameter limits returned ranges."""
        pool = build_v3_pool_with_ticks(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            tick_spacing=60,
            fee=3000,
            current_tick=-120,
            current_liquidity=10_000_000,
            current_sqrt_price_x96=get_sqrt_ratio_at_tick(-120),
            tick_ranges=[
                TickRangeDefinition(-180, -120, 10_000_000),
                TickRangeDefinition(-120, -60, 15_000_000),
                TickRangeDefinition(-60, 0, 20_000_000),
                TickRangeDefinition(0, 60, 25_000_000),
            ],
        )

        result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=True,
            max_ranges=2,
        )

        assert result is not None
        tick_ranges, _ = result
        assert len(tick_ranges) <= 3  # max_ranges + 1 for boundaries

    def test_cache_hit_returns_same_result(self, usdc: FakeToken, weth: FakeToken):
        """Test that cache returns consistent results for same pool state."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # First call populates cache
        result1 = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=True,
        )

        # Second call should hit cache
        result2 = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=True,
        )

        assert result1 is not None
        assert result2 is not None
        assert result1[0] == result2[0]
        assert result1[1] == result2[1]


class TestPoolStateToHopIntegration:
    """Tests for production V3 pool attributes needed by tick functions."""

    def test_pool_has_required_attributes(self, usdc: FakeToken, weth: FakeToken):
        """Test that production V3 pools have the attributes needed by tick functions."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
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

    def test_sparse_flag_causes_early_return(self, usdc: FakeToken, weth: FakeToken):
        """Test that sparse_liquidity_map flag causes immediate None return."""
        pool = create_two_range_v3_pool(
            address="0x1234567890123456789012345678901234567890",
            token0=usdc,  # type: ignore[arg-type]
            token1=weth,  # type: ignore[arg-type]
            current_tick=-60,
            lower_liquidity=10_000_000,
            upper_liquidity=20_000_000,
        )

        # Sparse pool should return None immediately
        pool._sparse_liquidity_map = True
        result = _v3_get_adjacent_tick_ranges(
            pool=pool,
            zero_for_one=True,
            max_ranges=3,
        )
        assert result is None


class TestFakeTickInfo:
    """Tests for FakeTickInfo dataclass."""

    def test_to_liquidity_at_tick_conversion(self):
        """Test conversion to LiquidityAtTick."""
        from tests.arbitrage.fake_pools import FakeTickInfo

        fake_tick = FakeTickInfo(liquidity_net=10_000_000, liquidity_gross=10_000_000)

        real_tick = fake_tick.to_liquidity_at_tick()

        assert real_tick.liquidity_net == 10_000_000
        assert real_tick.liquidity_gross == 10_000_000

    def test_immutability(self):
        """Test that FakeTickInfo is frozen (immutable)."""
        from tests.arbitrage.fake_pools import FakeTickInfo

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
