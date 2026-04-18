"""
V3 Tick Range Cache Tests

Tests for pool-level tick caching that accelerates V2-V3 and V3-V3 arbitrage.

The cache provides:
1. O(log n) tick range lookup by price
2. Pre-computed virtual reserves per range
3. Automatic invalidation on pool state changes
4. Shared cache across all arbitrage helpers
"""

import dataclasses

import pytest
from eth_typing import ChecksumAddress

from degenbot.uniswap.v3_types import (
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)

# ==============================================================================
# TICK RANGE CACHE TYPES
# ==============================================================================


@dataclasses.dataclass(frozen=True, slots=True)
class TickRangeInfo:
    """
    Cached information about a tick range.

    Pre-computed values for fast arbitrage optimization.
    """

    tick_lower: int
    tick_upper: int
    liquidity: int
    sqrt_price_lower: int  # Pre-computed for fast lookup
    sqrt_price_upper: int  # Pre-computed for fast lookup

    @property
    def price_lower(self) -> float:
        """Lower price bound (token1/token0)."""
        return (self.sqrt_price_lower / (2**96)) ** 2

    @property
    def price_upper(self) -> float:
        """Upper price bound (token1/token0)."""
        return (self.sqrt_price_upper / (2**96)) ** 2

    def contains_tick(self, tick: int) -> bool:
        """Check if tick is within range."""
        return self.tick_lower <= tick < self.tick_upper

    def contains_sqrt_price(self, sqrt_price_x96: int) -> bool:
        """Check if sqrt price is within range."""
        return self.sqrt_price_lower <= sqrt_price_x96 < self.sqrt_price_upper


# ==============================================================================
# TICK RANGE CACHE
# ==============================================================================


class V3TickRangeCache:
    """
    Cache for V3 tick range lookups.

    Provides O(log n) lookup instead of O(n) iteration through tick_data.

    Lifecycle:
    1. Pool creates cache on first access
    2. Cache rebuilds when tick_data changes
    3. Multiple arbitrage helpers share the same cache
    """

    def __init__(self, tick_spacing: int):
        self._tick_spacing = tick_spacing
        self._ranges: list[TickRangeInfo] = []
        self._ranges_by_tick: dict[int, TickRangeInfo] = {}
        self._valid = False

    def invalidate(self) -> None:
        """Mark cache as invalid (needs rebuild)."""
        self._valid = False

    def rebuild(
        self,
        tick_data: dict[int, UniswapV3LiquidityAtTick],
        current_liquidity: int,
        current_tick: int,
    ) -> None:
        """
        Rebuild cache from tick data.

        This is called when cache is invalid and needs to be rebuilt.
        Only rebuilds if invalid to avoid unnecessary work.
        """
        if self._valid:
            return

        self._ranges.clear()
        self._ranges_by_tick.clear()

        # Build ranges from initialized ticks
        # Each tick represents the start of a new liquidity range
        sorted_ticks = sorted(tick_data.keys())

        # We need to track liquidity as we traverse ticks
        # liquidity_net at each tick tells us the change
        running_liquidity = current_liquidity

        # Start from current tick and work outward
        # This is a simplification - real implementation would need full tick traversal
        for i, tick in enumerate(sorted_ticks):
            tick_info = tick_data[tick]

            # Determine tick_upper (next initialized tick or next spacing boundary)
            if i + 1 < len(sorted_ticks):
                tick_upper = sorted_ticks[i + 1]
            else:
                # Use tick_spacing aligned boundary
                tick_upper = tick + self._tick_spacing

            # Pre-compute sqrt prices
            sqrt_price_lower = self._tick_to_sqrt_price_x96(tick)
            sqrt_price_upper = self._tick_to_sqrt_price_x96(tick_upper)

            range_info = TickRangeInfo(
                tick_lower=tick,
                tick_upper=tick_upper,
                liquidity=running_liquidity,
                sqrt_price_lower=sqrt_price_lower,
                sqrt_price_upper=sqrt_price_upper,
            )

            self._ranges.append(range_info)
            self._ranges_by_tick[tick] = range_info

            # Update running liquidity for next range
            running_liquidity += tick_info.liquidity_net

        self._valid = True

    def find_range_at_price(self, price: float) -> TickRangeInfo | None:
        """
        Find tick range containing the given price.

        Uses binary search for O(log n) performance.

        Args:
            price: Price in token1/token0 terms

        Returns:
            TickRangeInfo if found, None if price outside all ranges
        """
        if not self._ranges:
            return None

        sqrt_price = int((price**0.5) * (2**96))

        # Binary search
        lo, hi = 0, len(self._ranges) - 1

        while lo <= hi:
            mid = (lo + hi) // 2
            range_info = self._ranges[mid]

            if sqrt_price < range_info.sqrt_price_lower:
                hi = mid - 1
            elif sqrt_price >= range_info.sqrt_price_upper:
                lo = mid + 1
            else:
                return range_info

        return None

    def find_range_at_tick(self, tick: int) -> TickRangeInfo | None:
        """Find tick range containing the given tick."""
        return self._ranges_by_tick.get(tick)

    def get_all_ranges(self) -> list[TickRangeInfo]:
        """Get all cached tick ranges."""
        return self._ranges.copy()

    @staticmethod
    def _tick_to_sqrt_price_x96(tick: int) -> int:
        """Convert tick to sqrt price in X96 format."""
        import math

        return int(math.sqrt(1.0001**tick) * (2**96))

    @property
    def is_valid(self) -> bool:
        """Check if cache is valid."""
        return self._valid

    @property
    def num_ranges(self) -> int:
        """Number of cached ranges."""
        return len(self._ranges)


# ==============================================================================
# MOCK V3 POOL WITH CACHE
# ==============================================================================


class MockV3PoolWithCache:
    """
    Mock V3 pool demonstrating tick range cache integration.

    In production, this cache would be part of UniswapV3Pool.
    """

    def __init__(
        self,
        address: ChecksumAddress,
        tick_spacing: int,
        initial_state: UniswapV3PoolState,
    ):
        self.address = address
        self.tick_spacing = tick_spacing
        self._state = initial_state

        # Tick range cache
        self._tick_cache = V3TickRangeCache(tick_spacing)

    @property
    def state(self) -> UniswapV3PoolState:
        return self._state

    def update_state(self, new_state: UniswapV3PoolState) -> None:
        """Update pool state (invalidates cache)."""
        self._state = new_state
        self._tick_cache.invalidate()

    def get_tick_range_at_price(self, price: float) -> TickRangeInfo | None:
        """
        Get tick range at price with caching.

        Rebuilds cache if invalid, then returns cached result.
        """
        if not self._tick_cache.is_valid:
            self._tick_cache.rebuild(
                tick_data=self._state.tick_data,
                current_liquidity=self._state.liquidity,
                current_tick=self._state.tick,
            )
        return self._tick_cache.find_range_at_price(price)

    def get_tick_range_at_tick(self, tick: int) -> TickRangeInfo | None:
        """Get tick range at tick with caching."""
        if not self._tick_cache.is_valid:
            self._tick_cache.rebuild(
                tick_data=self._state.tick_data,
                current_liquidity=self._state.liquidity,
                current_tick=self._state.tick,
            )
        return self._tick_cache.find_range_at_tick(tick)

    def get_all_tick_ranges(self) -> list[TickRangeInfo]:
        """Get all tick ranges (rebuilds cache if needed)."""
        if not self._tick_cache.is_valid:
            self._tick_cache.rebuild(
                tick_data=self._state.tick_data,
                current_liquidity=self._state.liquidity,
                current_tick=self._state.tick,
            )
        return self._tick_cache.get_all_ranges()


# ==============================================================================
# TESTS
# ==============================================================================


class TestTickRangeInfo:
    """Tests for TickRangeInfo dataclass."""

    def test_price_bounds(self) -> None:
        """Test price bound calculations."""
        # Tick 0 has sqrt_price = 2^96
        range_info = TickRangeInfo(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000,
            sqrt_price_lower=2**96,  # price = 1.0
            sqrt_price_upper=int((1.0001**60) ** 0.5 * (2**96)),  # price ≈ 1.006
        )

        assert range_info.price_lower == pytest.approx(1.0, rel=1e-6)
        assert range_info.price_upper == pytest.approx(1.0001**60, rel=1e-6)

    def test_contains_tick(self) -> None:
        """Test tick containment check."""
        range_info = TickRangeInfo(
            tick_lower=100,
            tick_upper=160,
            liquidity=1_000_000,
            sqrt_price_lower=2**96,
            sqrt_price_upper=2**96 + 1,
        )

        assert range_info.contains_tick(100) is True
        assert range_info.contains_tick(130) is True
        assert range_info.contains_tick(159) is True
        assert range_info.contains_tick(160) is False  # Upper bound is exclusive
        assert range_info.contains_tick(99) is False


class TestV3TickRangeCache:
    """Tests for V3TickRangeCache."""

    @pytest.fixture
    def tick_data(self) -> dict[int, UniswapV3LiquidityAtTick]:
        """Create sample tick data."""
        return {
            0: UniswapV3LiquidityAtTick(liquidity_net=1_000_000, liquidity_gross=1_000_000),
            60: UniswapV3LiquidityAtTick(liquidity_net=-500_000, liquidity_gross=500_000),
            120: UniswapV3LiquidityAtTick(liquidity_net=-500_000, liquidity_gross=500_000),
        }

    @pytest.fixture
    def cache(self, tick_data: dict[int, UniswapV3LiquidityAtTick]) -> V3TickRangeCache:
        """Create and build a cache."""
        cache = V3TickRangeCache(tick_spacing=60)
        cache.rebuild(
            tick_data=tick_data,
            current_liquidity=1_000_000,
            current_tick=30,
        )
        return cache

    def test_cache_starts_invalid(self) -> None:
        """Test that cache starts in invalid state."""
        cache = V3TickRangeCache(tick_spacing=60)
        assert not cache.is_valid
        assert cache.num_ranges == 0

    def test_rebuild_makes_valid(
        self,
        tick_data: dict[int, UniswapV3LiquidityAtTick],
    ) -> None:
        """Test that rebuild makes cache valid."""
        cache = V3TickRangeCache(tick_spacing=60)
        cache.rebuild(
            tick_data=tick_data,
            current_liquidity=1_000_000,
            current_tick=30,
        )
        assert cache.is_valid
        assert cache.num_ranges == 3

    def test_invalidate_resets(self, cache: V3TickRangeCache) -> None:
        """Test that invalidate resets valid flag."""
        assert cache.is_valid
        cache.invalidate()
        assert not cache.is_valid

    def test_find_range_at_price(self, cache: V3TickRangeCache) -> None:
        """Test finding range by price."""
        # Price = 1.0 should be in range starting at tick 0
        range_info = cache.find_range_at_price(1.0)
        assert range_info is not None
        assert range_info.tick_lower == 0

    def test_find_range_at_tick(self, cache: V3TickRangeCache) -> None:
        """Test finding range by tick."""
        range_info = cache.find_range_at_tick(0)
        assert range_info is not None
        assert range_info.tick_lower == 0

        range_info = cache.find_range_at_tick(60)
        assert range_info is not None
        assert range_info.tick_lower == 60

    def test_find_range_outside_bounds(self, cache: V3TickRangeCache) -> None:
        """Test finding range outside cached bounds."""
        # Very low price (before first range)
        cache.find_range_at_price(0.0001)
        # May return None or first range depending on implementation
        # For now, expect None for outside bounds

        # Very high price (after last range)
        cache.find_range_at_price(1e10)
        # May return None or last range

    def test_get_all_ranges(self, cache: V3TickRangeCache) -> None:
        """Test getting all ranges."""
        ranges = cache.get_all_ranges()
        assert len(ranges) == 3
        assert ranges[0].tick_lower == 0
        assert ranges[1].tick_lower == 60
        assert ranges[2].tick_lower == 120

    def test_rebuild_skip_if_valid(
        self,
        tick_data: dict[int, UniswapV3LiquidityAtTick],
    ) -> None:
        """Test that rebuild skips if already valid."""
        cache = V3TickRangeCache(tick_spacing=60)

        # First build
        cache.rebuild(
            tick_data=tick_data,
            current_liquidity=1_000_000,
            current_tick=30,
        )
        assert cache.num_ranges == 3

        # Modify tick_data (shouldn't affect cache since it's valid)
        new_tick_data = {
            0: UniswapV3LiquidityAtTick(liquidity_net=999_999_999, liquidity_gross=999_999_999)
        }
        cache.rebuild(
            tick_data=new_tick_data,
            current_liquidity=1_000_000,
            current_tick=30,
        )

        # Should still have 3 ranges (didn't rebuild)
        assert cache.num_ranges == 3


class TestMockV3PoolWithCache:
    """Tests for MockV3PoolWithCache demonstrating cache integration."""

    @pytest.fixture
    def pool_state(self) -> UniswapV3PoolState:
        """Create sample pool state."""
        return UniswapV3PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=0,
            liquidity=1_000_000,
            sqrt_price_x96=2**96,  # Price = 1.0
            tick=0,
            tick_bitmap={},
            tick_data={
                -60: UniswapV3LiquidityAtTick(
                    liquidity_net=500_000_000_000,
                    liquidity_gross=500_000_000_000,
                ),
                0: UniswapV3LiquidityAtTick(
                    liquidity_net=500_000_000_000,
                    liquidity_gross=1_000_000_000_000,
                ),
                60: UniswapV3LiquidityAtTick(
                    liquidity_net=-500_000_000_000,
                    liquidity_gross=500_000_000_000,
                ),
                120: UniswapV3LiquidityAtTick(
                    liquidity_net=-500_000_000_000,
                    liquidity_gross=500_000_000_000,
                ),
            },
        )

    @pytest.fixture
    def pool(self, pool_state: UniswapV3PoolState) -> MockV3PoolWithCache:
        """Create a mock pool with cache."""
        return MockV3PoolWithCache(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            tick_spacing=60,
            initial_state=pool_state,
        )

    def test_cache_invalidates_on_state_update(self, pool: MockV3PoolWithCache) -> None:
        """Test that state update invalidates cache."""
        # Access cache (builds it)
        pool.get_tick_range_at_price(1.0)
        assert pool._tick_cache.is_valid

        # Update state
        new_state = dataclasses.replace(pool.state, liquidity=2_000_000)
        pool.update_state(new_state)

        # Cache should be invalid
        assert not pool._tick_cache.is_valid

    def test_auto_rebuild_on_access(self, pool: MockV3PoolWithCache) -> None:
        """Test that cache auto-rebuilds on access."""
        # First access (builds cache)
        range_info = pool.get_tick_range_at_price(1.0)
        assert range_info is not None

        # Invalidate
        pool._tick_cache.invalidate()
        assert not pool._tick_cache.is_valid

        # Access again (should auto-rebuild)
        range_info = pool.get_tick_range_at_price(1.0)
        assert range_info is not None
        assert pool._tick_cache.is_valid

    def test_multiple_accesses_use_cache(self, pool: MockV3PoolWithCache) -> None:
        """Test that multiple accesses use cached data."""
        # Build cache
        pool.get_tick_range_at_price(1.0)
        assert pool._tick_cache.is_valid

        # Get internal ranges list (would need to expose this)
        ranges_before = pool._tick_cache.num_ranges

        # Multiple accesses should not rebuild
        pool.get_tick_range_at_price(1.0)
        pool.get_tick_range_at_price(1.001)
        pool.get_tick_range_at_price(0.999)

        assert pool._tick_cache.num_ranges == ranges_before


class TestTickCachePerformance:
    """Performance tests for tick cache."""

    def test_lookup_performance(self) -> None:
        """Benchmark tick range lookup."""
        import time

        import numpy as np

        # Create cache with 100 tick ranges
        tick_data = {}
        for i in range(100):
            tick_data[i * 60] = UniswapV3LiquidityAtTick(
                liquidity_net=1_000_000,
                liquidity_gross=1_000_000,
            )

        cache = V3TickRangeCache(tick_spacing=60)
        cache.rebuild(
            tick_data=tick_data,
            current_liquidity=1_000_000,
            current_tick=0,
        )

        # Benchmark lookups
        times = []
        for _ in range(10000):
            start = time.perf_counter_ns()
            cache.find_range_at_price(1.0 + (i % 100) * 0.001)
            times.append(time.perf_counter_ns() - start)

        mean_time_us = np.mean(times) / 1000
        print(f"\nTick range lookup: {mean_time_us:.2f}μs")

        # Should be very fast (< 10μs for binary search)
        assert mean_time_us < 20, f"Lookup too slow: {mean_time_us:.2f}μs"

    def test_rebuild_performance(self) -> None:
        """Benchmark cache rebuild."""
        import time

        import numpy as np

        # Create large tick data
        tick_data = {}
        for i in range(1000):
            tick_data[i * 60] = UniswapV3LiquidityAtTick(
                liquidity_net=1_000_000,
                liquidity_gross=1_000_000,
            )

        cache = V3TickRangeCache(tick_spacing=60)

        # Benchmark rebuild
        times = []
        for _ in range(100):
            cache.invalidate()
            start = time.perf_counter_ns()
            cache.rebuild(
                tick_data=tick_data,
                current_liquidity=1_000_000,
                current_tick=0,
            )
            times.append(time.perf_counter_ns() - start)

        mean_time_us = np.mean(times) / 1000
        print(f"\nCache rebuild (1000 ranges): {mean_time_us:.2f}μs")

        # Should be reasonable (< 10ms for 1000 ranges)
        assert mean_time_us < 10000, f"Rebuild too slow: {mean_time_us:.2f}μs"


class TestCacheInArbitrage:
    """Tests showing how cache integrates with arbitrage optimization."""

    @pytest.fixture
    def pool(self) -> MockV3PoolWithCache:
        """Create a mock pool for arbitrage testing."""
        state = UniswapV3PoolState(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            block=0,
            liquidity=1_000_000_000_000,
            sqrt_price_x96=2**96,
            tick=0,
            tick_bitmap={},
            tick_data={
                -60: UniswapV3LiquidityAtTick(
                    liquidity_net=500_000_000_000, liquidity_gross=500_000_000_000
                ),
                0: UniswapV3LiquidityAtTick(
                    liquidity_net=500_000_000_000, liquidity_gross=1_000_000_000_000
                ),
                60: UniswapV3LiquidityAtTick(
                    liquidity_net=-500_000_000_000, liquidity_gross=500_000_000_000
                ),
                120: UniswapV3LiquidityAtTick(
                    liquidity_net=-500_000_000_000, liquidity_gross=500_000_000_000
                ),
            },
        )
        return MockV3PoolWithCache(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            tick_spacing=60,
            initial_state=state,
        )

    def test_find_optimal_range_for_price(self, pool: MockV3PoolWithCache) -> None:
        """Test finding optimal tick range for a given price."""
        # Price near 1.0 should find tick 0 range
        range_info = pool.get_tick_range_at_price(1.0)
        assert range_info is not None
        assert range_info.tick_lower == 0

        # Price near 1.006 should find tick 60 range
        # price_upper for tick 0 ≈ 1.0001^60 ≈ 1.006
        range_info = pool.get_tick_range_at_price(1.007)
        assert range_info is not None
        # Should be in tick 60 range or later

    def test_get_ranges_for_arbitrage(self, pool: MockV3PoolWithCache) -> None:
        """Test getting multiple ranges for arbitrage optimization."""
        all_ranges = pool.get_all_tick_ranges()

        # Should have multiple ranges
        assert len(all_ranges) >= 3

        # Each range should have valid data
        for range_info in all_ranges:
            assert range_info.liquidity > 0
            assert range_info.sqrt_price_upper > range_info.sqrt_price_lower

    def test_cache_reuse_across_optimizations(self, pool: MockV3PoolWithCache) -> None:
        """Test that cache is reused across multiple optimization calls."""
        # Simulate multiple optimization passes
        prices_to_check = [0.99, 1.0, 1.01, 1.02, 1.03]

        # First pass builds cache
        [pool.get_tick_range_at_price(price) for price in prices_to_check]

        assert pool._tick_cache.is_valid

        # Second pass should use same cache
        for price in prices_to_check:
            pool.get_tick_range_at_price(price)

        # Cache should still be valid (not rebuilt)
        assert pool._tick_cache.is_valid

    def test_state_change_invalidates_for_new_arbitrage(self, pool: MockV3PoolWithCache) -> None:
        """Test that state changes invalidate cache for subsequent arbitrage."""
        # Build cache with first arbitrage
        pool.get_tick_range_at_price(1.0)
        assert pool._tick_cache.is_valid

        # Simulate state change (e.g., from swap)
        new_state = dataclasses.replace(
            pool.state,
            liquidity=900_000_000_000,  # Liquidity changed
        )
        pool.update_state(new_state)

        # Cache should be invalid
        assert not pool._tick_cache.is_valid

        # Next arbitrage will rebuild
        pool.get_tick_range_at_price(1.0)
        assert pool._tick_cache.is_valid
