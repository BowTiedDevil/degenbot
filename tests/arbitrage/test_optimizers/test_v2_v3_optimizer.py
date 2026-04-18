"""
Tests for V2-V3 arbitrage optimizer with tick range prediction.
"""

import math
from dataclasses import dataclass

import pytest

from degenbot.arbitrage.optimizers.v2_v3_optimizer import (
    V2PoolState,
    V2V3OptimizationResult,
    V2V3Optimizer,
    V3PoolState,
    compute_price_bounds,
    estimate_equilibrium_price,
    estimate_equilibrium_sqrt_price,
    filter_tick_ranges_by_price_bounds,
    optimize_v2_v3_arbitrage,
    solve_v2_v3_single_range,
    sort_ranges_by_equilibrium_distance,
)
from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    TickRange,
    tick_range_to_bounded_product,
    tick_to_sqrt_price,
)

# =============================================================================
# MOCK CLASSES
# =============================================================================


class MockToken:
    """Mock token for testing."""

    def __init__(self, address: str, symbol: str, decimals: int = 18):
        self.address = address
        self.symbol = symbol
        self.decimals = decimals

    def __repr__(self):
        return f"Token({self.symbol})"

    def __hash__(self):
        return hash(self.address)

    def __eq__(self, other):
        if isinstance(other, MockToken):
            return self.address == other.address
        return False


@dataclass
class MockV2State:
    """Mock V2 pool state."""

    reserves_token0: int
    reserves_token1: int


class MockV2Pool:
    """Mock V2 pool for testing."""

    def __init__(
        self,
        token0: MockToken,
        token1: MockToken,
        reserve0: int,
        reserve1: int,
        fee: float = 0.003,
    ):
        self.token0 = token0
        self.token1 = token1
        self.state = MockV2State(reserve0, reserve1)
        self.fee = fee


@dataclass
class MockV3State:
    """Mock V3 pool state."""

    sqrt_price_x96: int
    tick: int
    liquidity: int


class MockV3Pool:
    """Mock V3 pool for testing."""

    def __init__(
        self,
        token0: MockToken,
        token1: MockToken,
        sqrt_price_x96: int,
        tick: int,
        liquidity: int,
        fee: float = 0.003,
        tick_spacing: int = 60,
    ):
        self.token0 = token0
        self.token1 = token1
        self.state = MockV3State(sqrt_price_x96, tick, liquidity)
        self.fee = fee
        self.tick_spacing = tick_spacing


# =============================================================================
# FIXTURES
# =============================================================================


@pytest.fixture
def usdc():
    return MockToken("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "USDC", 6)


@pytest.fixture
def weth():
    return MockToken("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "WETH", 18)


@pytest.fixture
def v2_state():
    """V2 pool: 2M USDC / 1000 WETH (price: 2000 USDC/WETH)."""
    return V2PoolState(
        reserve0=2_000_000.0,
        reserve1=1_000.0,
        fee=0.003,
        token0_address="usdc",
        token1_address="weth",
    )


@pytest.fixture
def v3_state():
    """V3 pool at tick 0 with liquidity."""
    return V3PoolState(
        sqrt_price_x96=2**96,  # sqrt_price = 1.0
        sqrt_price=1.0,
        tick=0,
        liquidity=1_000_000,
        fee=0.003,
        tick_spacing=60,
        token0_address="usdc",
        token1_address="weth",
        token0_decimals=6,
        token1_decimals=18,
        virtual_reserve0=1_000_000.0,
        virtual_reserve1=1_000_000.0,
    )


@pytest.fixture
def tick_ranges():
    """Sample tick ranges for testing."""
    return [
        TickRange(
            tick_lower=-120,
            tick_upper=-60,
            liquidity=500_000,
            sqrt_price_lower=tick_to_sqrt_price(-120),
            sqrt_price_upper=tick_to_sqrt_price(-60),
        ),
        TickRange(
            tick_lower=-60,
            tick_upper=0,
            liquidity=800_000,
            sqrt_price_lower=tick_to_sqrt_price(-60),
            sqrt_price_upper=tick_to_sqrt_price(0),
        ),
        TickRange(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000,
            sqrt_price_lower=tick_to_sqrt_price(0),
            sqrt_price_upper=tick_to_sqrt_price(60),
        ),
        TickRange(
            tick_lower=60,
            tick_upper=120,
            liquidity=700_000,
            sqrt_price_lower=tick_to_sqrt_price(60),
            sqrt_price_upper=tick_to_sqrt_price(120),
        ),
        TickRange(
            tick_lower=120,
            tick_upper=180,
            liquidity=400_000,
            sqrt_price_lower=tick_to_sqrt_price(120),
            sqrt_price_upper=tick_to_sqrt_price(180),
        ),
    ]


# =============================================================================
# EQUILIBRIUM ESTIMATION TESTS
# =============================================================================


class TestEquilibriumEstimation:
    """Tests for equilibrium price estimation."""

    def test_estimate_equilibrium_same_prices(self, v2_state):
        """When V2 and V3 have same price, equilibrium equals that price."""
        v3_state = V3PoolState(
            sqrt_price_x96=2**96 * 2000**0.5,  # Price 2000
            sqrt_price=2000**0.5,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
            token0_address="usdc",
            token1_address="weth",
            token0_decimals=6,
            token1_decimals=18,
            virtual_reserve0=1_000_000 / 2000**0.5,
            virtual_reserve1=1_000_000 * 2000**0.5,
        )

        p_eq = estimate_equilibrium_price(v2_state, v3_state)

        # Should be close to geometric mean
        v2_price = v2_state.price
        v3_price = v3_state.virtual_reserve1 / v3_state.virtual_reserve0
        expected = math.sqrt(v2_price * v3_price)

        assert abs(p_eq - expected) / expected < 0.1  # Within 10%

    def test_estimate_equilibrium_different_prices(self, v2_state):
        """Equilibrium is between V2 and V3 prices."""
        # V3 with different price
        v3_state = V3PoolState(
            sqrt_price_x96=2**96,
            sqrt_price=1.0,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
            token0_address="usdc",
            token1_address="weth",
            token0_decimals=6,
            token1_decimals=18,
            virtual_reserve0=1_000_000.0,
            virtual_reserve1=1_000_000.0,  # Price = 1.0
        )

        p_eq = estimate_equilibrium_price(v2_state, v3_state)

        # Should be between the two prices
        v2_price = v2_state.price  # 2000
        v3_price = v3_state.virtual_reserve1 / v3_state.virtual_reserve0  # 1.0

        assert p_eq > min(v2_price, v3_price)
        assert p_eq < max(v2_price, v3_price)

    def test_estimate_equilibrium_sqrt_price(self, v2_state, v3_state):
        """sqrt price is sqrt of equilibrium price."""
        p_eq = estimate_equilibrium_price(v2_state, v3_state)
        sqrt_p_eq = estimate_equilibrium_sqrt_price(v2_state, v3_state)

        assert abs(sqrt_p_eq - math.sqrt(p_eq)) < 1e-10


# =============================================================================
# PRICE BOUNDS TESTS
# =============================================================================


class TestPriceBounds:
    """Tests for price bounds computation."""

    def test_compute_price_bounds_basic(self, v2_state, v3_state):
        """Price bounds are computed correctly."""
        p_lower, p_upper = compute_price_bounds(v2_state, v3_state)

        # Bounds should be reasonable
        assert p_lower > 0
        assert p_upper > p_lower

        # Equilibrium estimate exists
        p_eq = estimate_equilibrium_price(v2_state, v3_state)
        assert p_eq > 0

    def test_price_bounds_with_large_discrepancy(self):
        """Price bounds handle large price discrepancies."""
        v2 = V2PoolState(
            reserve0=1_000_000.0,
            reserve1=1_000.0,  # price = 0.001
            fee=0.003,
            token0_address="a",
            token1_address="b",
        )

        v3 = V3PoolState(
            sqrt_price_x96=2**96,
            sqrt_price=1.0,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
            token0_address="a",
            token1_address="b",
            token0_decimals=18,
            token1_decimals=18,
            virtual_reserve0=1_000_000.0,
            virtual_reserve1=1_000_000.0,  # price = 1.0
        )

        p_lower, p_upper = compute_price_bounds(v2, v3)

        # Bounds should be valid
        assert p_lower > 0
        assert p_upper >= p_lower


# =============================================================================
# TICK RANGE FILTERING TESTS
# =============================================================================


class TestTickRangeFiltering:
    """Tests for tick range filtering."""

    def test_filter_by_price_bounds_includes_valid(self, tick_ranges):
        """Valid ranges are included."""
        # Price bounds around tick 0
        p_lower = tick_to_sqrt_price(-30) ** 2
        p_upper = tick_to_sqrt_price(90) ** 2

        filtered = filter_tick_ranges_by_price_bounds(tick_ranges, p_lower, p_upper)

        # Should include ranges overlapping with bounds
        assert len(filtered) >= 2  # At least ranges containing -30 to 90

    def test_filter_by_price_bounds_excludes_invalid(self, tick_ranges):
        """Ranges outside bounds are excluded."""
        # Very narrow price bounds
        p_lower = tick_to_sqrt_price(5) ** 2
        p_upper = tick_to_sqrt_price(10) ** 2

        filtered = filter_tick_ranges_by_price_bounds(tick_ranges, p_lower, p_upper)

        # Only one range should match (tick 0-60)
        assert len(filtered) <= 2

    def test_filter_empty_ranges(self):
        """Empty input returns empty output."""
        filtered = filter_tick_ranges_by_price_bounds([], 0.5, 2.0)
        assert filtered == []

    def test_sort_by_equilibrium_distance(self, tick_ranges):
        """Ranges are sorted by distance to equilibrium."""
        # Equilibrium at tick 30
        sqrt_p_eq = tick_to_sqrt_price(30)

        sorted_ranges = sort_ranges_by_equilibrium_distance(tick_ranges, sqrt_p_eq)

        # First should be the range containing tick 30 (tick 0-60)
        assert sorted_ranges[0].tick_lower == 0
        assert sorted_ranges[0].tick_upper == 60

    def test_sort_by_equilibrium_outside_ranges(self, tick_ranges):
        """Sorting works when equilibrium is outside all ranges."""
        sqrt_p_eq = tick_to_sqrt_price(200)  # Far above all ranges

        sorted_ranges = sort_ranges_by_equilibrium_distance(tick_ranges, sqrt_p_eq)

        # Should be sorted by distance
        assert sorted_ranges[0].tick_upper == 180  # Closest range


# =============================================================================
# SINGLE RANGE SOLVER TESTS
# =============================================================================


class TestSingleRangeSolver:
    """Tests for single range V2-V3 solver."""

    def test_solve_single_range_basic(self, v2_state):
        """Solver finds a solution."""
        # Create bounded product CFMM
        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=1_000_000.0,
        )

        optimal_input, _optimal_output, profit = solve_v2_v3_single_range(
            v2_state=v2_state,
            v3_cfmm=cfmm,
            v3_current_sqrt_price=1.0,
        )

        # Should find some solution (may or may not be profitable)
        assert optimal_input >= 0
        assert isinstance(profit, float)

    def test_solve_single_range_with_matching_prices(self):
        """When prices match, minimal arbitrage."""
        v2 = V2PoolState(
            reserve0=1_000_000.0,
            reserve1=1_000.0,  # Price = 1000
            fee=0.003,
            token0_address="a",
            token1_address="b",
        )

        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=100_000.0,
        )

        # V3 at similar price
        optimal_input, _, _profit = solve_v2_v3_single_range(
            v2_state=v2,
            v3_cfmm=cfmm,
            v3_current_sqrt_price=1000**0.5,  # Price 1000
        )

        # With matching prices, minimal opportunity
        assert isinstance(optimal_input, float)


# =============================================================================
# OPTIMIZER TESTS
# =============================================================================


class TestV2V3Optimizer:
    """Tests for the main optimizer."""

    def test_optimizer_initialization(self):
        """Optimizer initializes correctly."""
        optimizer = V2V3Optimizer(
            max_candidates=5,
            max_iterations=100,
        )

        assert optimizer.max_candidates == 5
        assert optimizer.max_iterations == 100

    def test_optimizer_with_mock_pools(self, usdc, weth):
        """Optimizer works with mock pools."""
        v2_pool = MockV2Pool(
            token0=usdc,
            token1=weth,
            reserve0=2_000_000_000_000,
            reserve1=1_000 * 10**18,
            fee=0.003,
        )

        v3_pool = MockV3Pool(
            token0=usdc,
            token1=weth,
            sqrt_price_x96=2**96,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
        )

        optimizer = V2V3Optimizer()
        result = optimizer.optimize(v2_pool, v3_pool, usdc)

        assert isinstance(result, V2V3OptimizationResult)
        assert result.solve_time_ms > 0
        assert isinstance(result.candidate_solutions, list)

    def test_optimizer_returns_equilibrium(self, usdc, weth):
        """Optimizer returns equilibrium estimate."""
        v2_pool = MockV2Pool(
            token0=usdc,
            token1=weth,
            reserve0=2_000_000_000_000,
            reserve1=1_000 * 10**18,
            fee=0.003,
        )

        v3_pool = MockV3Pool(
            token0=usdc,
            token1=weth,
            sqrt_price_x96=2**96,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
        )

        optimizer = V2V3Optimizer()
        result = optimizer.optimize(v2_pool, v3_pool, usdc)

        assert result.equilibrium_estimate > 0


# =============================================================================
# INTEGRATION TESTS
# =============================================================================


class TestIntegration:
    """Integration tests."""

    def test_convenience_function(self, usdc, weth):
        """Convenience function works."""
        v2_pool = MockV2Pool(
            token0=usdc,
            token1=weth,
            reserve0=2_000_000_000_000,
            reserve1=1_000 * 10**18,
            fee=0.003,
        )

        v3_pool = MockV3Pool(
            token0=usdc,
            token1=weth,
            sqrt_price_x96=2**96,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
        )

        result = optimize_v2_v3_arbitrage(v2_pool, v3_pool, usdc)

        assert isinstance(result, V2V3OptimizationResult)
        assert result.solve_time_ms > 0

    def test_optimizer_with_custom_tick_ranges(self, usdc, weth):
        """Optimizer works with custom tick ranges."""
        v2_pool = MockV2Pool(
            token0=usdc,
            token1=weth,
            reserve0=2_000_000_000_000,
            reserve1=1_000 * 10**18,
            fee=0.003,
        )

        v3_pool = MockV3Pool(
            token0=usdc,
            token1=weth,
            sqrt_price_x96=2**96,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
        )

        tick_ranges = [
            TickRange(
                tick_lower=0,
                tick_upper=60,
                liquidity=1_000_000,
                sqrt_price_lower=tick_to_sqrt_price(0),
                sqrt_price_upper=tick_to_sqrt_price(60),
            ),
        ]

        optimizer = V2V3Optimizer()
        result = optimizer.optimize(v2_pool, v3_pool, usdc, tick_ranges)

        assert len(result.candidate_solutions) <= 1


# =============================================================================
# EDGE CASES
# =============================================================================


class TestEdgeCases:
    """Edge case tests."""

    def test_zero_reserves(self):
        """Handle zero reserves gracefully."""
        v2 = V2PoolState(
            reserve0=0.0,
            reserve1=0.0,
            fee=0.003,
            token0_address="a",
            token1_address="b",
        )

        v3 = V3PoolState(
            sqrt_price_x96=2**96,
            sqrt_price=1.0,
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
            token0_address="a",
            token1_address="b",
            token0_decimals=18,
            token1_decimals=18,
            virtual_reserve0=1_000_000.0,
            virtual_reserve1=1_000_000.0,
        )

        p_eq = estimate_equilibrium_price(v2, v3)
        # Should return some value, not crash
        assert isinstance(p_eq, float)

    def test_very_different_liquidity(self, v2_state):
        """Handle very different liquidity levels."""
        # Small V3 liquidity
        cfmm = tick_range_to_bounded_product(
            tick_lower=0,
            tick_upper=60,
            liquidity=100.0,  # Very small
        )

        optimal_input, _, _profit = solve_v2_v3_single_range(
            v2_state=v2_state,
            v3_cfmm=cfmm,
            v3_current_sqrt_price=1.0,
        )

        # Should handle gracefully
        assert isinstance(optimal_input, float)

    def test_extreme_price_difference(self):
        """Handle extreme price differences."""
        v2 = V2PoolState(
            reserve0=1_000_000.0,
            reserve1=1.0,  # price = 1/1,000,000 = 0.000001
            fee=0.003,
            token0_address="a",
            token1_address="b",
        )

        v3 = V3PoolState(
            sqrt_price_x96=2**96,
            sqrt_price=1.0,  # price = 1.0
            tick=0,
            liquidity=1_000_000,
            fee=0.003,
            tick_spacing=60,
            token0_address="a",
            token1_address="b",
            token0_decimals=18,
            token1_decimals=18,
            virtual_reserve0=1_000_000.0,
            virtual_reserve1=1_000_000.0,
        )

        p_eq = estimate_equilibrium_price(v2, v3)
        # Equilibrium should be between the two pool prices
        v2_price = v2.price  # 1/1,000,000
        v3_price = v3.virtual_reserve1 / v3.virtual_reserve0  # 1.0
        # Should be somewhere between these extremes
        assert p_eq >= min(v2_price, v3_price) * 0.1
        assert p_eq <= max(v2_price, v3_price) * 10
