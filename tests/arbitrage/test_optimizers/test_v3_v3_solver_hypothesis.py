"""
Property-based tests for V3-V3 arbitrage solver.

Tests the Rust V3-V3 solver with randomized sqrt prices, liquidity values,
and tick ranges to verify correctness across the parameter space.
"""

import math
from typing import Any

import hypothesis
import hypothesis.strategies as st
import pytest

from degenbot.degenbot_rs import mobius
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
)

from .conftest import make_rust_v3_hop as make_v3_hop

# ==============================================================================
# Hypothesis Strategies
# ==============================================================================

# Valid tick range (within V3 bounds)
tick_strategy = st.integers(min_value=MIN_TICK + 1000, max_value=MAX_TICK - 1000)

# Liquidity values (reasonable range for testing)
liquidity_strategy = st.floats(min_value=1e15, max_value=1e21, allow_nan=False, allow_infinity=False)

# Fee values (standard V3 fee tiers)
fee_strategy = st.sampled_from([0.0001, 0.0005, 0.003, 0.01])

# Price multiplier for creating price differences
price_mult_strategy = st.floats(min_value=0.8, max_value=1.2, allow_nan=False, allow_infinity=False)

# Tick spacing for valid ranges
tick_spacing_strategy = st.sampled_from([1, 10, 60, 200])


# ==============================================================================
# Helpers
# ==============================================================================


def sqrt_price_from_tick(tick: int) -> float:
    """Convert tick to sqrt price as float."""
    return float(get_sqrt_ratio_at_tick(tick)) / (2**96)


def make_single_range_hop(
    liquidity: float,
    sqrt_price: float,
    fee: float,
    zero_for_one: bool,
    range_mult: float = 0.5,
) -> Any:
    """Create a single tick range hop centered at sqrt_price."""
    sqrt_lower = sqrt_price * (1 - range_mult)
    sqrt_upper = sqrt_price * (1 + range_mult)
    return make_v3_hop(liquidity, sqrt_price, sqrt_lower, sqrt_upper, fee, zero_for_one=zero_for_one)


def make_wide_range_hop(
    liquidity: float,
    sqrt_price: float,
    fee: float,
    zero_for_one: bool,
) -> Any:
    """Create a wide tick range hop for testing fast path."""
    sqrt_lower = sqrt_price * 0.1  # Very wide
    sqrt_upper = sqrt_price * 10.0  # Very wide
    return make_v3_hop(liquidity, sqrt_price, sqrt_lower, sqrt_upper, fee, zero_for_one=zero_for_one)


# ==============================================================================
# Property Tests: Single Range V3-V3
# ==============================================================================


class TestV3V3SingleRangeProperties:
    """Property tests for single-range V3-V3 arbitrage."""

    @hypothesis.given(
        base_price=st.floats(min_value=100.0, max_value=10000.0, allow_nan=False, allow_infinity=False),
        price_spread=st.floats(min_value=0.01, max_value=0.5, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_single_range_finds_profit_with_price_spread(
        self,
        base_price: float,
        price_spread: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: Single-range V3-V3 finds profit when price spread exceeds fees.

        When pool A has price (1 + spread) times pool B, arbitrage should be profitable
        if the spread exceeds the total fees (2x fee for round trip).
        """
        sqrt_pa = math.sqrt(base_price * (1 + price_spread))
        sqrt_pb = math.sqrt(base_price)

        # Wide ranges to ensure single-range fast path
        hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Profit should exist when spread > 2 * fee (approximately)
        min_profitable_spread = 2 * fee * 1.01  # Small buffer for numerical precision

        if price_spread > min_profitable_spread:
            assert result.success, f"Expected success for spread={price_spread}, fee={fee}"
            assert result.profit > 0, f"Expected profit for spread={price_spread}, fee={fee}"

    @hypothesis.given(
        price=st.floats(min_value=100.0, max_value=10000.0, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_single_range_no_profit_identical_prices(
        self,
        price: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: No profit when both pools have identical prices.

        With fees, arbitrage between identical-priced pools is unprofitable.
        """
        sqrt_p = math.sqrt(price)

        hop1 = make_wide_range_hop(liquidity, sqrt_p, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_p, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Should not find profitable arbitrage
        assert not result.success or result.profit == 0

    @hypothesis.given(
        base_price=st.floats(min_value=100.0, max_value=10000.0, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_single_range_zero_iterations_for_wide_ranges(
        self,
        base_price: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: Wide single ranges use fast path (0 iterations).

        When tick ranges are wide enough that no crossing is needed,
        the solver should use the Möbius fast path.
        """
        sqrt_pa = math.sqrt(base_price * 1.1)  # 10% price difference
        sqrt_pb = math.sqrt(base_price)

        hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Wide ranges should use fast path
        if result.success:
            assert result.iterations == 0


class TestV3V3ProfitProperties:
    """Property tests for profit computation."""

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_profit_increases_with_price_spread(
        self,
        base_price: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: Larger price spread produces larger profit.

        For a fixed liquidity and fee, profit should be monotonically
        increasing with the price spread between pools.
        """
        profits = []
        spreads = [0.05, 0.10, 0.20]  # 5%, 10%, 20% spread

        for spread in spreads:
            sqrt_pa = math.sqrt(base_price * (1 + spread))
            sqrt_pb = math.sqrt(base_price)

            hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
            hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

            seq1 = mobius.RustV3TickRangeSequence([hop1])
            seq2 = mobius.RustV3TickRangeSequence([hop2])

            result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
            profits.append(result.profit if result.success else 0)

        # Larger spread → larger profit (when profitable)
        if profits[0] > 0:
            assert profits[1] > profits[0], f"Expected profit[1]={profits[1]} > profit[0]={profits[0]}"
            assert profits[2] > profits[1], f"Expected profit[2]={profits[2]} > profit[1]={profits[1]}"

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.05, max_value=0.2, allow_nan=False, allow_infinity=False),
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_profit_scales_with_liquidity(
        self,
        base_price: float,
        spread: float,
        fee: float,
    ):
        """
        Property: Profit scales linearly with liquidity.

        For a fixed price spread, doubling liquidity should approximately
        double the profit (with small deviations from fee rounding).
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        # Small liquidity
        hop1_small = make_wide_range_hop(1e18, sqrt_pa, fee, zero_for_one=True)
        hop2_small = make_wide_range_hop(1e18, sqrt_pb, fee, zero_for_one=False)
        seq1_small = mobius.RustV3TickRangeSequence([hop1_small])
        seq2_small = mobius.RustV3TickRangeSequence([hop2_small])
        result_small = mobius.RustMobiusOptimizer().solve_v3_v3(seq1_small, seq2_small)

        # Double liquidity
        hop1_double = make_wide_range_hop(2e18, sqrt_pa, fee, zero_for_one=True)
        hop2_double = make_wide_range_hop(2e18, sqrt_pb, fee, zero_for_one=False)
        seq1_double = mobius.RustV3TickRangeSequence([hop1_double])
        seq2_double = mobius.RustV3TickRangeSequence([hop2_double])
        result_double = mobius.RustMobiusOptimizer().solve_v3_v3(seq1_double, seq2_double)

        if result_small.success and result_double.success:
            ratio = result_double.profit / result_small.profit if result_small.profit > 0 else 0
            # Should be approximately 2x (within 5% tolerance)
            if result_small.profit > 1e-10:
                assert 1.9 < ratio < 2.1, f"Expected ~2x profit, got {ratio}x"

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.1, max_value=0.3, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_lower_fee_higher_profit(
        self,
        base_price: float,
        spread: float,
        liquidity: float,
    ):
        """
        Property: Lower fees produce higher profit.

        For the same price spread and liquidity, lower fees should yield
        higher profit.
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        profits = {}
        for fee in [0.01, 0.003, 0.0005]:  # 1%, 0.3%, 0.05%
            hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
            hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)
            seq1 = mobius.RustV3TickRangeSequence([hop1])
            seq2 = mobius.RustV3TickRangeSequence([hop2])
            result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)
            profits[fee] = result.profit if result.success else 0

        # Lower fee → higher profit
        if profits[0.01] > 0 and profits[0.003] > 0:
            assert profits[0.003] > profits[0.01], "Lower fee should give higher profit"
        if profits[0.003] > 0 and profits[0.0005] > 0:
            assert profits[0.0005] > profits[0.003], "Lower fee should give higher profit"


class TestV3V3BoundsProperties:
    """Property tests for optimal input bounds."""

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.05, max_value=0.2, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_optimal_input_positive_and_finite(
        self,
        base_price: float,
        spread: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: Optimal input is always positive and finite when successful.
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if result.success:
            assert result.optimal_input > 0
            assert math.isfinite(result.optimal_input)
            assert result.profit > 0
            assert math.isfinite(result.profit)

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.1, max_value=0.3, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
        max_input_fraction=st.floats(min_value=0.01, max_value=0.5, allow_nan=False, allow_infinity=False),
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_max_input_constraint_respected(
        self,
        base_price: float,
        spread: float,
        liquidity: float,
        fee: float,
        max_input_fraction: float,
    ):
        """
        Property: max_input constraint is respected.

        When a max_input constraint is applied, the optimal input should
        not exceed it.
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        # First get unconstrained result
        result_unconstrained = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if result_unconstrained.success and result_unconstrained.optimal_input > 0:
            # Apply constraint
            max_input = result_unconstrained.optimal_input * max_input_fraction
            result_constrained = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2, max_input)

            if result_constrained.success:
                assert result_constrained.optimal_input <= max_input * 1.001  # Small tolerance
                assert result_constrained.profit <= result_unconstrained.profit


class TestV3V3MultiRangeProperties:
    """Property tests for multi-range V3-V3 (tick crossing)."""

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.05, max_value=0.15, allow_nan=False, allow_infinity=False),
        liquidity1=liquidity_strategy,
        liquidity2=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_multi_range_does_not_panic(
        self,
        base_price: float,
        spread: float,
        liquidity1: float,
        liquidity2: float,
        fee: float,
    ):
        """
        Property: Multi-range V3-V3 handles edge cases gracefully.

        The solver should never panic or return invalid results for any
        valid input configuration.
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        # Create two ranges per hop
        hop1_r1 = make_v3_hop(
            liquidity1, sqrt_pa, sqrt_pa * 0.9, sqrt_pa * 1.05, fee, zero_for_one=True
        )
        hop1_r2 = make_v3_hop(
            liquidity2, sqrt_pa * 1.05, sqrt_pa * 1.0, sqrt_pa * 1.1, fee, zero_for_one=True
        )

        hop2_r1 = make_v3_hop(
            liquidity1, sqrt_pb, sqrt_pb * 0.95, sqrt_pb * 1.1, fee, zero_for_one=False
        )
        hop2_r2 = make_v3_hop(
            liquidity2, sqrt_pb * 0.95, sqrt_pb * 0.9, sqrt_pb * 1.0, fee, zero_for_one=False
        )

        seq1 = mobius.RustV3TickRangeSequence([hop1_r1, hop1_r2])
        seq2 = mobius.RustV3TickRangeSequence([hop2_r1, hop2_r2])

        # Should not panic
        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        # Result should be valid
        assert result.iterations >= 0
        assert math.isfinite(result.optimal_input)
        assert math.isfinite(result.profit)


class TestV3V3Invariants:
    """Property tests for solver invariants."""

    @hypothesis.given(
        base_price=st.floats(min_value=500.0, max_value=5000.0, allow_nan=False, allow_infinity=False),
        spread=st.floats(min_value=0.05, max_value=0.2, allow_nan=False, allow_infinity=False),
        liquidity=liquidity_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_optimal_input_yields_maximum_profit(
        self,
        base_price: float,
        spread: float,
        liquidity: float,
        fee: float,
    ):
        """
        Property: Profit at optimal input is maximum.

        Verify that the profit at optimal_input is greater than profit
        at nearby values (local maximum property).
        """
        sqrt_pa = math.sqrt(base_price * (1 + spread))
        sqrt_pb = math.sqrt(base_price)

        hop1 = make_wide_range_hop(liquidity, sqrt_pa, fee, zero_for_one=True)
        hop2 = make_wide_range_hop(liquidity, sqrt_pb, fee, zero_for_one=False)

        seq1 = mobius.RustV3TickRangeSequence([hop1])
        seq2 = mobius.RustV3TickRangeSequence([hop2])

        result = mobius.RustMobiusOptimizer().solve_v3_v3(seq1, seq2)

        if result.success and result.optimal_input > 0:
            # Test points around optimal
            x_opt = result.optimal_input
            x_test_points = [x_opt * 0.5, x_opt * 0.8, x_opt * 1.2, x_opt * 1.5]

            for x_test in x_test_points:
                if x_test > 0:
                    # Simulate profit at test point
                    hs1 = hop1.to_hop_state()
                    hs2 = hop2.to_hop_state()
                    test_output = mobius.py_simulate_path(x_test, [hs1, hs2])
                    test_profit = test_output - x_test

                    # Optimal should be >= test point (within tolerance)
                    # Allow small tolerance for numerical precision
                    rel_diff = (result.profit - test_profit) / max(abs(result.profit), 1e-10)
                    assert rel_diff >= -1e-6, (
                        f"Profit at optimal ({result.profit}) < profit at {x_test:.2e} ({test_profit})"
                    )
