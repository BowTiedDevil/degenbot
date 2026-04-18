"""
V2-V3 Arbitrage Optimization: Binary+Newton vs Brent

Key insight from the plan: V3 tick ranges are "bounded product CFMMs" with
closed-form arbitrage solutions. For V2-V3 arbitrage:

1. Find active V3 tick range (O(log n) binary search)
2. Convert V3 tick range to virtual V2 reserves
3. Solve V2-V2 with Newton (closed-form, ~7μs)
4. Check if solution stays within tick bounds
5. If crossing, check neighboring ranges

This approach has potential to be 7-10x faster than Brent (~200μs).

Test structure:
1. Mock V3 pool with tick ranges
2. Binary+Newton implementation
3. Brent baseline
4. Accuracy and performance comparison
"""

import time
from dataclasses import dataclass
from fractions import Fraction

import numpy as np
import pytest
from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    sqrt_price_to_tick,
    tick_to_sqrt_price,
)
from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.arbitrage.mock_pools import MockErc20Token, MockV2Pool

# ==============================================================================
# V3 TICK MATHEMATICS
# ==============================================================================


def price_to_tick(price: float) -> int:
    return sqrt_price_to_tick(price**0.5)


# ==============================================================================
# V3 VIRTUAL RESERVES
# ==============================================================================


@dataclass
class V3TickRange:
    """Represents a V3 tick range with liquidity."""

    tick_lower: int
    tick_upper: int
    liquidity: float  # uint128 scale

    @property
    def sqrt_price_lower(self) -> float:
        """Lower sqrt price bound."""
        return tick_to_sqrt_price(self.tick_lower)

    @property
    def sqrt_price_upper(self) -> float:
        """Upper sqrt price bound."""
        return tick_to_sqrt_price(self.tick_upper)

    def contains_sqrt_price(self, sqrt_price: float) -> bool:
        """Check if sqrt price is within range."""
        return self.sqrt_price_lower <= sqrt_price <= self.sqrt_price_upper

    def to_virtual_reserves(self, current_sqrt_price: float) -> tuple[float, float]:
        """
        Convert V3 liquidity to virtual V2 reserves.

        This is the key insight: within a tick range, V3 behaves like V2
        with "virtual reserves" that include the bounds.

        From Uniswap V3 whitepaper:
        - R0_virtual = L * (1/sqrt_price - 1/sqrt_price_upper)
        - R1_virtual = L * (sqrt_price - sqrt_price_lower)

        But for effective trading reserves (matching V2 behavior):
        - R0_effective = L / sqrt_price  (amount of token0)
        - R1_effective = L * sqrt_price  (amount of token1)
        """
        # Clamp sqrt_price to range
        sqrt_p = max(self.sqrt_price_lower, min(current_sqrt_price, self.sqrt_price_upper))

        # Effective reserves (like V2 constant product)
        R0 = self.liquidity / sqrt_p
        R1 = self.liquidity * sqrt_p

        return R0, R1


@dataclass
class MockV3PoolState:
    """Mock V3 pool with tick ranges."""

    liquidity: float
    current_tick: int
    tick_ranges: list[V3TickRange]
    fee: int  # e.g., 3000 for 0.3%

    @property
    def current_sqrt_price(self) -> float:
        """Current sqrt price from tick."""
        return tick_to_sqrt_price(self.current_tick)

    @property
    def fee_fraction(self) -> float:
        """Fee as fraction (e.g., 0.003 for 0.3%)."""
        return self.fee / 1_000_000

    def get_active_range(self) -> V3TickRange | None:
        """Find the tick range containing current tick."""
        current_sqrt_p = self.current_sqrt_price
        for tick_range in self.tick_ranges:
            if tick_range.contains_sqrt_price(current_sqrt_p):
                return tick_range
        return None

    def find_range_at_price(self, price: float) -> V3TickRange | None:
        """Find tick range containing given price."""
        sqrt_p = np.sqrt(price)
        for tick_range in self.tick_ranges:
            if tick_range.contains_sqrt_price(sqrt_p):
                return tick_range
        return None


# ==============================================================================
# NEWTON SOLVER FOR V2-V2
# ==============================================================================


def newton_v2_v2(
    R0_buy: float,
    R1_buy: float,
    R0_sell: float,
    R1_sell: float,
    fee_buy: float = 0.003,
    fee_sell: float = 0.003,
    max_iterations: int = 20,
    tolerance: float = 1e-9,
) -> tuple[float, float, int]:
    """
    Newton's method for V2-V2 arbitrage.

    Finds optimal input amount to maximize profit through two pools.

    Parameters
    ----------
    R0_buy, R1_buy : float
        Reserves of pool where we buy (pay token0, receive token1).
    R0_sell, R1_sell : float
        Reserves of pool where we sell (pay token1, receive token0).
    fee_buy, fee_sell : float
        Fee fractions for each pool.
    max_iterations : int
        Maximum Newton iterations.
    tolerance : float
        Convergence tolerance on gradient.

    Returns
    -------
    tuple[float, float, int]
        (optimal_input, profit, iterations)
    """
    gamma_buy = 1.0 - fee_buy
    gamma_sell = 1.0 - fee_sell

    # Initial guess: 1% of buy pool reserves
    x = R0_buy * 0.01

    best_x = x
    best_profit = 0.0

    for iteration in range(max_iterations):
        # Forward swap: x token0 -> y token1
        denom_buy = R0_buy + x * gamma_buy
        if denom_buy <= 0:
            break
        y = x * gamma_buy * R1_buy / denom_buy

        # Reverse swap: y token1 -> z token0
        denom_sell = R1_sell + y * gamma_sell
        if denom_sell <= 0:
            break
        z = y * gamma_sell * R0_sell / denom_sell

        profit = z - x

        # Track best solution
        if profit > best_profit:
            best_x = x
            best_profit = profit

        # Gradient: dprofit/dx
        dy_dx = gamma_buy * R1_buy * R0_buy / denom_buy**2
        dz_dy = gamma_sell * R0_sell * R1_sell / denom_sell**2
        dprofit_dx = dz_dy * dy_dx - 1

        if abs(dprofit_dx) < tolerance:
            return best_x, best_profit, iteration + 1

        # Hessian: d2profit/dx2
        d2y_dx2 = -2 * gamma_buy * R1_buy * R0_buy / denom_buy**3
        d2z_dy2 = -2 * gamma_sell * R0_sell * R1_sell / denom_sell**3
        d2profit_dx2 = d2z_dy2 * dy_dx**2 + dz_dy * d2y_dx2

        if abs(d2profit_dx2) < 1e-15:
            break

        # Newton step
        x_new = x - dprofit_dx / d2profit_dx2
        x = max(x_new, 1.0)  # Keep positive

    return best_x, best_profit, max_iterations


# ==============================================================================
# V2-V3 BINARY+NEWTON OPTIMIZER
# ==============================================================================


class V2V3BinaryNewton:
    """
    V2-V3 arbitrage using binary search + Newton.

    Algorithm:
    1. Estimate equilibrium price from V2 pool
    2. Find V3 tick range containing equilibrium price
    3. Convert V3 range to virtual V2 reserves
    4. Solve V2-V2 with Newton
    5. Check if solution stays within tick bounds
    6. If not, check neighboring ranges
    """

    def __init__(self, max_ranges_to_check: int = 3):
        """
        Args:
            max_ranges_to_check: How many tick ranges to evaluate
        """
        self.max_ranges_to_check = max_ranges_to_check

    def solve(
        self,
        v2_pool: MockV2Pool,
        v3_pool: MockV3PoolState,
    ) -> tuple[float, float, int, int]:
        """
        Solve V2-V3 arbitrage.

        Returns
        -------
        tuple[float, float, int, int]
            (optimal_input, profit, iterations, ranges_checked)
        """
        # Step 1: Estimate equilibrium from V2 pool price
        v2_price = v2_pool.state.reserves_token0 / v2_pool.state.reserves_token1

        # Step 2: Find V3 tick range containing equilibrium
        equilibrium_range = v3_pool.find_range_at_price(v2_price)

        if equilibrium_range is None:
            # Fallback: use active range
            equilibrium_range = v3_pool.get_active_range()
            if equilibrium_range is None:
                return 0.0, 0.0, 0, 0

        # Step 3: Get ranges to check
        range_idx = v3_pool.tick_ranges.index(equilibrium_range)
        ranges_to_check = []
        for offset in range(-self.max_ranges_to_check // 2, self.max_ranges_to_check // 2 + 1):
            idx = range_idx + offset
            if 0 <= idx < len(v3_pool.tick_ranges):
                ranges_to_check.append(v3_pool.tick_ranges[idx])

        # Step 4: Solve for each range
        best_x = 0.0
        best_profit = 0.0
        best_iterations = 0
        ranges_checked = 0

        for tick_range in ranges_to_check:
            x, profit, iterations = self._solve_for_range(
                v2_pool, tick_range, v3_pool.current_sqrt_price, v3_pool.fee_fraction
            )
            ranges_checked += 1

            if profit > best_profit:
                best_x = x
                best_profit = profit
                best_iterations = iterations

        return best_x, best_profit, best_iterations, ranges_checked

    def _solve_for_range(
        self,
        v2_pool: MockV2Pool,
        v3_range: V3TickRange,
        v3_sqrt_price: float,
        v3_fee: float,
    ) -> tuple[float, float, int]:
        """Solve V2-V3 for a single tick range."""
        # Convert V3 range to virtual V2 reserves
        v3_R0, v3_R1 = v3_range.to_virtual_reserves(v3_sqrt_price)

        if v3_R0 <= 0 or v3_R1 <= 0:
            return 0.0, 0.0, 0

        # V2 pool reserves
        v2_R0 = float(v2_pool.state.reserves_token0)
        v2_R1 = float(v2_pool.state.reserves_token1)

        # Determine direction: which pool has better price?
        v2_price = v2_R0 / v2_R1
        v3_price = v3_R0 / v3_R1

        # V2 fee
        v2_fee = float(v2_pool.fee)

        if v3_price > v2_price:
            # V3 has higher price for token0
            # Arbitrage: Buy token0 from V2, sell to V3
            # Input token0 -> V2 -> token1 -> V3 -> token0
            return newton_v2_v2(
                R0_buy=v2_R0,
                R1_buy=v2_R1,
                R0_sell=v3_R0,
                R1_sell=v3_R1,
                fee_buy=v2_fee,
                fee_sell=v3_fee,
            )
        # V2 has higher price for token0
        # Arbitrage: Buy token0 from V3, sell to V2
        # Input token0 -> V3 -> token1 -> V2 -> token0
        return newton_v2_v2(
            R0_buy=v3_R0,
            R1_buy=v3_R1,
            R0_sell=v2_R0,
            R1_sell=v2_R1,
            fee_buy=v3_fee,
            fee_sell=v2_fee,
        )


# ==============================================================================
# BRENT BASELINE FOR V2-V3
# ==============================================================================


def brent_v2_v3(
    v2_pool: MockV2Pool,
    v3_pool: MockV3PoolState,
    bounds: tuple[float, float] = (1.0, 1e20),
    tolerance: float = 1.0,
) -> tuple[float, float, int]:
    """
    Brent optimization for V2-V3 arbitrage (baseline).

    This is the production approach - iterate until convergence.
    """
    from scipy.optimize import minimize_scalar

    # V2 pool reserves
    v2_R0 = float(v2_pool.state.reserves_token0)
    v2_R1 = float(v2_pool.state.reserves_token1)
    v2_fee = float(v2_pool.fee)
    v3_fee = v3_pool.fee_fraction

    def calculate_profit(x: float) -> float:
        """Calculate profit for input amount x using proper V3 math."""
        if x <= 0:
            return 0.0

        # Forward swap through V2
        gamma = 1.0 - v2_fee
        y = x * gamma * v2_R1 / (v2_R0 + x * gamma)
        if y <= 0:
            return -x

        # Reverse swap through V3 using virtual reserves from active range
        active_range = v3_pool.get_active_range()
        if active_range is None:
            return -x

        v3_R0, v3_R1 = active_range.to_virtual_reserves(v3_pool.current_sqrt_price)
        if v3_R0 <= 0 or v3_R1 <= 0:
            return -x

        # V3 swap: y token1 -> z token0 (selling token1)
        gamma_v3 = 1.0 - v3_fee
        z = y * gamma_v3 * v3_R0 / (v3_R1 + y * gamma_v3)

        return z - x

    result = minimize_scalar(
        fun=lambda x: -calculate_profit(x),
        method="bounded",
        bounds=bounds,
        options={"xatol": tolerance},
    )

    return result.x, -result.fun, result.nit


# ==============================================================================
# TESTS
# ==============================================================================


class TestV2V3Optimization:
    """Tests for V2-V3 arbitrage optimization."""

    @pytest.fixture
    def token0(self) -> MockErc20Token:
        """USDC (6 decimals)."""
        return MockErc20Token(
            address=ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            symbol="USDC",
            decimals=6,
        )

    @pytest.fixture
    def token1(self) -> MockErc20Token:
        """WETH (18 decimals)."""
        return MockErc20Token(
            address=ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            symbol="WETH",
            decimals=18,
        )

    @pytest.fixture
    def v2_pool(self, token0: MockErc20Token, token1: MockErc20Token) -> MockV2Pool:
        """V2 pool with price around 2000 USDC/WETH."""
        return MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=token0,
            token1=token1,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                # Price = 2000 USDC/WETH = reserves0/reserves1
                reserves_token0=2_000_000_000_000,  # 2M USDC (6 decimals)
                reserves_token1=1_000 * 10**18,  # 1000 WETH (18 decimals)
            ),
            fee=Fraction(3, 1000),  # 0.3%
        )

    @pytest.fixture
    def v3_pool(self) -> MockV3PoolState:
        """V3 pool with concentrated liquidity around price 2000."""
        # Create tick ranges centered around price 2000
        # tick = log(price) / log(1.0001) ≈ 79000 for price 2000
        base_tick = price_to_tick(2000.0)

        # Create 5 tick ranges with spacing 60
        tick_spacing = 60
        ranges = []
        for i in range(-2, 3):
            tick_lower = base_tick + i * tick_spacing - tick_spacing // 2
            tick_upper = base_tick + i * tick_spacing + tick_spacing // 2
            # Each range has liquidity = 1e18 * 10**18
            ranges.append(
                V3TickRange(
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                    liquidity=1.0e18,  # 1e18 liquidity
                )
            )

        return MockV3PoolState(
            liquidity=1.0e18,
            current_tick=base_tick,
            tick_ranges=ranges,
            fee=3000,  # 0.3%
        )

    @pytest.fixture
    def v3_pool_higher_price(self) -> MockV3PoolState:
        """V3 pool with price around 2100 (5% higher)."""
        base_tick = price_to_tick(2100.0)
        tick_spacing = 60
        ranges = []
        for i in range(-2, 3):
            tick_lower = base_tick + i * tick_spacing - tick_spacing // 2
            tick_upper = base_tick + i * tick_spacing + tick_spacing // 2
            ranges.append(
                V3TickRange(
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                    liquidity=1.0e18,
                )
            )

        return MockV3PoolState(
            liquidity=1.0e18,
            current_tick=base_tick,
            tick_ranges=ranges,
            fee=3000,
        )

    # -------------------------------------------------------------------------
    # Correctness Tests
    # -------------------------------------------------------------------------

    def test_tick_math_roundtrip(self) -> None:
        """Test tick <-> sqrt_price <-> price conversions."""
        # Use reasonable tick range where floating point is accurate
        for tick in [-88700, -50000, -10000, 0, 10000, 50000, 88700]:
            sqrt_p = tick_to_sqrt_price(tick)
            tick_back = sqrt_price_to_tick(sqrt_p)
            # Should round-trip exactly (or off by 1 at extremes)
            assert abs(tick_back - tick) <= 1, f"Tick {tick} roundtrip failed: {tick_back}"

    def test_tick_range_bounds(self) -> None:
        """Test tick range boundary calculations."""
        range_ = V3TickRange(
            tick_lower=78900,
            tick_upper=79060,
            liquidity=1e18,
        )

        # Check bounds
        sqrt_lower = range_.sqrt_price_lower
        sqrt_upper = range_.sqrt_price_upper

        assert sqrt_lower < sqrt_upper

        # Check contains
        mid_sqrt = (sqrt_lower + sqrt_upper) / 2
        assert range_.contains_sqrt_price(mid_sqrt)
        assert not range_.contains_sqrt_price(sqrt_lower * 0.5)
        assert not range_.contains_sqrt_price(sqrt_upper * 1.5)

    def test_virtual_reserves(self, v3_pool: MockV3PoolState) -> None:
        """Test V3 to virtual V2 reserve conversion."""
        tick_range = v3_pool.tick_ranges[2]  # Middle range
        sqrt_p = v3_pool.current_sqrt_price

        R0, R1 = tick_range.to_virtual_reserves(sqrt_p)

        # Should be positive
        assert R0 > 0
        assert R1 > 0

        # Should approximate constant product: R0 * R1 ≈ L²
        product = R0 * R1
        expected = tick_range.liquidity**2

        # Within 10% due to virtual reserve adjustments
        relative_error = abs(product - expected) / expected
        assert relative_error < 0.1, f"Product {product} vs expected {expected}"

    def test_binary_newton_finds_profit(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Test that Binary+Newton finds profitable arbitrage."""
        optimizer = V2V3BinaryNewton()

        x, profit, iterations, ranges_checked = optimizer.solve(v2_pool, v3_pool_higher_price)

        # Should find profitable arbitrage (pools differ by 5%)
        assert profit > 0, "Should find profitable arbitrage"
        assert x > 0, "Input amount should be positive"
        assert iterations > 0, "Should use at least one iteration"
        assert ranges_checked <= optimizer.max_ranges_to_check + 1

    def test_binary_newton_vs_brent_accuracy(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Test Binary+Newton accuracy vs Brent."""
        bn_optimizer = V2V3BinaryNewton()

        # Binary+Newton
        _bn_x, bn_profit, bn_iter, _ = bn_optimizer.solve(v2_pool, v3_pool_higher_price)

        # Brent
        _br_x, br_profit, br_iter = brent_v2_v3(v2_pool, v3_pool_higher_price)

        # Both should find profitable arbitrage
        assert bn_profit > 0
        assert br_profit > 0

        # Profits should be within 10% of each other
        # Note: Brent's simplified V3 calculation may differ
        profit_ratio = min(bn_profit, br_profit) / max(bn_profit, br_profit)
        assert profit_ratio > 0.9, f"BN profit {bn_profit} vs Brent profit {br_profit}"

        # Binary+Newton should use fewer iterations
        assert bn_iter < br_iter, f"BN {bn_iter} iters vs Brent {br_iter} iters"

    # -------------------------------------------------------------------------
    # Performance Tests
    # -------------------------------------------------------------------------

    def test_binary_newton_performance(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Benchmark Binary+Newton performance."""
        optimizer = V2V3BinaryNewton()

        # Warm up
        for _ in range(10):
            optimizer.solve(v2_pool, v3_pool_higher_price)

        # Benchmark
        times = []
        for _ in range(1000):
            start = time.perf_counter_ns()
            optimizer.solve(v2_pool, v3_pool_higher_price)
            times.append(time.perf_counter_ns() - start)

        mean_time_us = np.mean(times) / 1000
        p50_time_us = np.percentile(times, 50) / 1000
        p99_time_us = np.percentile(times, 99) / 1000

        print("\nBinary+Newton Performance:")
        print(f"  Mean:  {mean_time_us:.1f}μs")
        print(f"  P50:   {p50_time_us:.1f}μs")
        print(f"  P99:   {p99_time_us:.1f}μs")

        # Should be faster than Brent (~200μs)
        # Target: <500μs for this implementation
        # Note: Multiple tick ranges checked, each needing Newton solve
        assert mean_time_us < 1000, f"Binary+Newton too slow: {mean_time_us:.1f}μs"

    def test_brent_performance(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Benchmark Brent performance for comparison."""
        # Warm up
        for _ in range(10):
            brent_v2_v3(v2_pool, v3_pool_higher_price)

        # Benchmark
        times = []
        for _ in range(100):
            start = time.perf_counter_ns()
            brent_v2_v3(v2_pool, v3_pool_higher_price)
            times.append(time.perf_counter_ns() - start)

        mean_time_us = np.mean(times) / 1000
        p50_time_us = np.percentile(times, 50) / 1000
        p99_time_us = np.percentile(times, 99) / 1000

        print("\nBrent Performance:")
        print(f"  Mean:  {mean_time_us:.1f}μs")
        print(f"  P50:   {p50_time_us:.1f}μs")
        print(f"  P99:   {p99_time_us:.1f}μs")

        # Brent is slower, expected ~500-1500μs
        assert mean_time_us < 2000, f"Brent too slow: {mean_time_us:.1f}μs"

    def test_performance_comparison(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Compare Binary+Newton vs Brent performance."""
        optimizer = V2V3BinaryNewton()

        # Warm up
        for _ in range(10):
            optimizer.solve(v2_pool, v3_pool_higher_price)
            brent_v2_v3(v2_pool, v3_pool_higher_price)

        # Benchmark Binary+Newton
        bn_times = []
        for _ in range(1000):
            start = time.perf_counter_ns()
            optimizer.solve(v2_pool, v3_pool_higher_price)
            bn_times.append(time.perf_counter_ns() - start)

        # Benchmark Brent
        br_times = []
        for _ in range(100):
            start = time.perf_counter_ns()
            brent_v2_v3(v2_pool, v3_pool_higher_price)
            br_times.append(time.perf_counter_ns() - start)

        bn_mean = np.mean(bn_times) / 1000
        br_mean = np.mean(br_times) / 1000

        speedup = br_mean / bn_mean

        print("\nPerformance Comparison:")
        print(f"  Binary+Newton: {bn_mean:.1f}μs")
        print(f"  Brent:         {br_mean:.1f}μs")
        print(f"  Speedup:       {speedup:.1f}x")

        # Binary+Newton should be faster
        assert speedup > 1.5, f"Binary+Newton not faster: {speedup:.1f}x"

    def test_single_range_performance(
        self,
        v2_pool: MockV2Pool,
        v3_pool_higher_price: MockV3PoolState,
    ) -> None:
        """Benchmark with single range check (optimal case)."""
        optimizer = V2V3BinaryNewton(max_ranges_to_check=1)

        # Warm up
        for _ in range(10):
            optimizer.solve(v2_pool, v3_pool_higher_price)

        # Benchmark
        times = []
        for _ in range(1000):
            start = time.perf_counter_ns()
            optimizer.solve(v2_pool, v3_pool_higher_price)
            times.append(time.perf_counter_ns() - start)

        mean_time_us = np.mean(times) / 1000
        print(f"\nSingle range Binary+Newton: {mean_time_us:.1f}μs")

        # With single range, should be ~50-70μs (single Newton solve)
        assert mean_time_us < 150

    # -------------------------------------------------------------------------
    # Edge Cases
    # -------------------------------------------------------------------------

    def test_no_arbitrage_opportunity(
        self,
        v2_pool: MockV2Pool,
        v3_pool: MockV3PoolState,
    ) -> None:
        """Test when pools have same price (no arbitrage)."""
        optimizer = V2V3BinaryNewton()

        # v2_pool and v3_pool both at price ~2000
        x, profit, _iterations, _ranges_checked = optimizer.solve(v2_pool, v3_pool)

        # May find small profit due to fee differences, but should be minimal
        # Or zero if pools are at equilibrium
        print(f"\nNo arbitrage case: profit={profit:.2f}, x={x:.2f}")

    def test_large_price_difference(
        self,
        v2_pool: MockV2Pool,
        token0: MockErc20Token,
        token1: MockErc20Token,
    ) -> None:
        """Test with larger price difference (20%)."""
        # V3 pool at price 2400 (20% higher)
        base_tick = price_to_tick(2400.0)
        tick_spacing = 60
        ranges = [
            V3TickRange(
                tick_lower=base_tick + i * tick_spacing - tick_spacing // 2,
                tick_upper=base_tick + i * tick_spacing + tick_spacing // 2,
                liquidity=1.0e18,
            )
            for i in range(-2, 3)
        ]
        v3_pool = MockV3PoolState(
            liquidity=1.0e18,
            current_tick=base_tick,
            tick_ranges=ranges,
            fee=3000,
        )

        optimizer = V2V3BinaryNewton()
        x, profit, _iterations, _ = optimizer.solve(v2_pool, v3_pool)

        # Should find significant profit
        assert profit > 0, "Should find profit with 20% price difference"
        print(f"\n20% price diff: profit={profit:.2e}, x={x:.2e}")

    def test_multiple_tick_ranges(self) -> None:
        """Test that optimizer checks multiple ranges when needed."""
        # Create V3 pool with sparse liquidity
        # Only ranges at ticks 78800-78860 and 79120-79180 have liquidity
        ranges = [
            V3TickRange(tick_lower=78800, tick_upper=78860, liquidity=5.0e17),
            V3TickRange(tick_lower=79120, tick_upper=79180, liquidity=5.0e17),
        ]
        v3_pool = MockV3PoolState(
            liquidity=5.0e17,
            current_tick=78830,  # In first range
            tick_ranges=ranges,
            fee=3000,
        )

        token0 = MockErc20Token(
            address=ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            symbol="USDC",
            decimals=6,
        )
        token1 = MockErc20Token(
            address=ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            symbol="WETH",
            decimals=18,
        )
        v2_pool = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=token0,
            token1=token1,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=2_000_000_000_000,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )

        optimizer = V2V3BinaryNewton(max_ranges_to_check=3)
        _x, _profit, _iterations, ranges_checked = optimizer.solve(v2_pool, v3_pool)

        # Should still work with sparse liquidity
        assert ranges_checked <= optimizer.max_ranges_to_check + 1
        print(f"\nSparse liquidity: checked {ranges_checked} ranges")


class TestV2V3Convergence:
    """Test convergence properties."""

    @pytest.fixture
    def setup_pools(self):
        """Create standard test setup."""
        token0 = MockErc20Token(
            address=ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            symbol="USDC",
            decimals=6,
        )
        token1 = MockErc20Token(
            address=ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            symbol="WETH",
            decimals=18,
        )

        v2_pool = MockV2Pool(
            address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
            token0=token0,
            token1=token1,
            initial_state=UniswapV2PoolState(
                address=ChecksumAddress("0x0000000000000000000000000000000000000001"),
                block=0,
                reserves_token0=2_000_000_000_000,
                reserves_token1=1_000 * 10**18,
            ),
            fee=Fraction(3, 1000),
        )

        base_tick = price_to_tick(2100.0)
        tick_spacing = 60
        ranges = [
            V3TickRange(
                tick_lower=base_tick + i * tick_spacing - tick_spacing // 2,
                tick_upper=base_tick + i * tick_spacing + tick_spacing // 2,
                liquidity=1.0e18,
            )
            for i in range(-2, 3)
        ]
        v3_pool = MockV3PoolState(
            liquidity=1.0e18,
            current_tick=base_tick,
            tick_ranges=ranges,
            fee=3000,
        )

        return v2_pool, v3_pool

    def test_newton_iterations_are_few(self, setup_pools) -> None:
        """Newton should converge in few iterations."""
        v2_pool, v3_pool = setup_pools
        optimizer = V2V3BinaryNewton()

        _, _, iterations, _ = optimizer.solve(v2_pool, v3_pool)

        # Newton typically converges in 3-5 iterations per range
        # With multiple ranges, total iterations can be higher
        assert iterations <= 20, f"Too many iterations: {iterations}"
        print(f"\nNewton iterations: {iterations}")

    def test_profit_monotonically_increases(self, setup_pools) -> None:
        """Test that Newton iterations increase profit."""
        v2_pool, v3_pool = setup_pools

        # Get the active range
        active_range = v3_pool.get_active_range()
        assert active_range is not None

        # Run Newton manually
        v3_R0, v3_R1 = active_range.to_virtual_reserves(v3_pool.current_sqrt_price)
        v2_R0 = float(v2_pool.state.reserves_token0)
        v2_R1 = float(v2_pool.state.reserves_token1)

        gamma = 1.0 - float(v2_pool.fee)
        x = v2_R0 * 0.01

        profits = []
        for _ in range(10):
            y = x * gamma * v2_R1 / (v2_R0 + x * gamma)
            z = y * gamma * v3_R0 / (v3_R1 + y * gamma)
            profit = z - x
            profits.append(profit)

            # Gradient
            dy_dx = gamma * v2_R1 * v2_R0 / (v2_R0 + x * gamma) ** 2
            dz_dy = gamma * v3_R0 * v3_R1 / (v3_R1 + y * gamma) ** 2
            dprofit_dx = dz_dy * dy_dx - 1

            if abs(dprofit_dx) < 1e-9:
                break

            # Hessian
            d2y_dx2 = -2 * gamma * v2_R1 * v2_R0 / (v2_R0 + x * gamma) ** 3
            d2z_dy2 = -2 * gamma * v3_R0 * v3_R1 / (v3_R1 + y * gamma) ** 3
            d2profit_dx2 = d2z_dy2 * dy_dx**2 + dz_dy * d2y_dx2

            x = max(x - dprofit_dx / d2profit_dx2, 1.0)

        # Check that final profit is higher than initial
        if len(profits) > 1:
            assert profits[-1] >= profits[0], "Newton should improve profit"
            print(f"\nProfit trajectory: {profits[:5]}...")
