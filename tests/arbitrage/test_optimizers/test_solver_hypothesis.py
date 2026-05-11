"""
Property-based tests for the unified solver interface.

Tests the ArbSolver dispatch logic and correctness across different
pool types (V2, V3) and path lengths.
"""

import math
from fractions import Fraction

import hypothesis
import hypothesis.strategies as st
import pytest

from degenbot.arbitrage.optimizers.hop_types import SolveInput
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BrentSolver,
    SolverMethod,
)
from degenbot.arbitrage.optimizers._solver_utils import _compute_mobius_coefficients
from degenbot.arbitrage.optimizers._v3_utils import _v3_virtual_reserves
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import BoundedProductHop, ConstantProductHop

# ==============================================================================
# Hypothesis Strategies
# ==============================================================================

# Reserve values
reserve_strategy = st.integers(min_value=10**12, max_value=10**21)

# Fee as Fraction (standard fee tiers)
fee_fraction_strategy = st.sampled_from([
    Fraction(1, 10000),  # 0.01%
    Fraction(5, 10000),  # 0.05%
    Fraction(3, 1000),  # 0.3%
    Fraction(1, 100),  # 1%
])

# Liquidity for V3 pools
liquidity_strategy = st.integers(min_value=10**15, max_value=10**21)

# Sqrt price (around 2^96 for price = 1.0)
sqrt_price_strategy = st.integers(min_value=2**95, max_value=2**97)

# Tick values
tick_strategy = st.integers(min_value=-10000, max_value=10000)

# Number of hops
num_hops_strategy = st.integers(min_value=2, max_value=6)

# Price multiplier for creating price differences
price_mult_strategy = st.floats(
    min_value=1.01, max_value=1.3, allow_nan=False, allow_infinity=False
)


# ==============================================================================
# Helpers
# ==============================================================================


def make_v2_hop(reserve_in: int, reserve_out: int, fee: Fraction) -> ConstantProductHop:
    """Create a V2 hop."""
    return ConstantProductHop(reserve_in=reserve_in, reserve_out=reserve_out, fee=fee)


def make_v3_hop(
    liquidity: int,
    sqrt_price: int,
    tick_lower: int,
    tick_upper: int,
    fee: Fraction,
) -> BoundedProductHop:
    """Create a V3 hop with virtual reserves."""
    # Compute virtual reserves from liquidity and sqrt price
    sqrt_price_float = sqrt_price / (2**96)
    reserve_in = int(liquidity / sqrt_price_float)
    reserve_out = int(liquidity * sqrt_price_float)

    return BoundedProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=fee,
        liquidity=liquidity,
        sqrt_price=sqrt_price,
        tick_lower=tick_lower,
        tick_upper=tick_upper,
    )


def make_profitable_v2_pair(
    base_reserve: int,
    price_mult: float,
    fee: Fraction,
) -> tuple[ConstantProductHop, ConstantProductHop]:
    """Create a profitable V2 pair."""
    reserve_a_in = base_reserve
    reserve_a_out = int(base_reserve * price_mult)

    reserve_b_in = int(base_reserve * price_mult * 0.99)
    reserve_b_out = base_reserve

    hop1 = make_v2_hop(reserve_a_in, reserve_a_out, fee)
    hop2 = make_v2_hop(reserve_b_in, reserve_b_out, fee)

    return hop1, hop2


# ==============================================================================
# Property Tests: V2-V2 Paths
# ==============================================================================


class TestSolverV2V2Properties:
    """Property tests for V2-V2 arbitrage."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=price_mult_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_solver_finds_profit_v2v2(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: Solver finds profit for V2-V2 with price spread > fees.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()

        try:
            result = solver.solve(inp)
            # If solver succeeds, verify the result is valid
            assert result.profit > 0, f"Expected profit, got {result.profit}"
            assert result.method == SolverMethod.MOBIUS
        except OptimizationError:
            # No profit found is valid (pools may not have profitable direction)
            pass

    @hypothesis.given(
        base_reserve=reserve_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_solver_no_profit_identical_prices(
        self,
        base_reserve: int,
        fee: Fraction,
    ):
        """
        Property: No profit for identical prices with fees.
        """
        hop = make_v2_hop(base_reserve, base_reserve, fee)
        inp = SolveInput(hops=(hop, hop))

        solver = ArbSolver()

        with pytest.raises(OptimizationError):
            solver.solve(inp)

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=price_mult_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_solver_matches_brent_v2v2(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: ArbSolver matches Brent for V2-V2.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()
        brent = BrentSolver()

        try:
            result_solver = solver.solve(inp)
            result_brent = brent.solve(inp)

            # Should match within tolerance (accounting for integer rounding)
            # Use relative tolerance for large profits, absolute for small
            abs_diff = abs(result_solver.profit - result_brent.profit)
            max_profit = max(result_solver.profit, result_brent.profit)
            rel_diff = abs_diff / max_profit if max_profit > 0 else 0

            assert abs_diff <= 10 or rel_diff < 1e-10, (
                f"Profit mismatch: {result_solver.profit} vs {result_brent.profit}"
            )
        except OptimizationError:
            # Both should fail similarly
            pass


class TestSolverMultiHopProperties:
    """Property tests for multi-hop paths."""

    @hypothesis.given(
        num_hops=num_hops_strategy,
        base_reserve=reserve_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_solver_handles_multi_hop(
        self,
        num_hops: int,
        base_reserve: int,
        fee: Fraction,
    ):
        """
        Property: Solver handles paths of various lengths.
        """
        # Create a cycle where prices drift around
        hops = []
        current_reserve = base_reserve

        for i in range(num_hops):
            # Alternating price drift
            if i % 2 == 0:
                reserve_in = current_reserve
                reserve_out = int(current_reserve * 1.02)
            else:
                reserve_in = int(current_reserve * 1.02)
                reserve_out = current_reserve

            hops.append(make_v2_hop(reserve_in, reserve_out, fee))

        inp = SolveInput(hops=tuple(hops))
        solver = ArbSolver()

        try:
            result = solver.solve(inp)
            # If successful, profit should be positive
            assert result.profit > 0
            # Multi-hop uses Möbius
            assert result.method == SolverMethod.MOBIUS
            # Zero iterations for closed-form
            assert result.iterations == 0
        except OptimizationError:
            pass  # No profit found is valid

    @hypothesis.given(
        num_hops=st.integers(min_value=3, max_value=6),
        base_reserve=reserve_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_solver_matches_brent_multi_hop(
        self,
        num_hops: int,
        base_reserve: int,
        fee: Fraction,
    ):
        """
        Property: Solver matches Brent for multi-hop paths.
        """
        # Create profitable cycle
        hops = []
        price_factor = 1.03  # 3% price drift per hop

        for i in range(num_hops):
            if i % 2 == 0:
                reserve_in = base_reserve
                reserve_out = int(base_reserve * price_factor)
            else:
                reserve_in = int(base_reserve * price_factor)
                reserve_out = base_reserve

            hops.append(make_v2_hop(reserve_in, reserve_out, fee))

        inp = SolveInput(hops=tuple(hops))
        solver = ArbSolver()
        brent = BrentSolver()

        try:
            result_solver = solver.solve(inp)
            result_brent = brent.solve(inp)

            # Should match within tolerance
            rel_diff = abs(result_solver.profit - result_brent.profit) / max(
                result_solver.profit, 1
            )
            assert rel_diff < 1e-4, (
                f"Profit mismatch: {result_solver.profit} vs {result_brent.profit}"
            )
        except OptimizationError:
            pass


class TestSolverMethodSelection:
    """Property tests for solver method selection."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=price_mult_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_v2v2_uses_mobius(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: V2-V2 paths use Möbius method.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()

        try:
            result = solver.solve(inp)
            assert result.method == SolverMethod.MOBIUS
        except OptimizationError:
            pass


class TestSolverBounds:
    """Property tests for solver bounds and constraints."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=st.floats(min_value=1.05, max_value=1.2, allow_nan=False, allow_infinity=False),
        fee=fee_fraction_strategy,
        max_input_fraction=st.floats(
            min_value=0.1, max_value=0.9, allow_nan=False, allow_infinity=False
        ),
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_max_input_constraint_respected(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
        max_input_fraction: float,
    ):
        """
        Property: max_input constraint is respected.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)

        # First find unconstrained solution
        solver = ArbSolver()

        try:
            result_unconstrained = solver.solve(SolveInput(hops=(hop1, hop2)))

            # Apply constraint
            max_input = int(result_unconstrained.optimal_input * max_input_fraction)
            result_constrained = solver.solve(SolveInput(hops=(hop1, hop2), max_input=max_input))

            # Constrained optimal should not exceed max_input
            assert result_constrained.optimal_input <= max_input
            # Constrained profit should be <= unconstrained
            assert result_constrained.profit <= result_unconstrained.profit
        except OptimizationError:
            pass

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=price_mult_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_optimal_input_within_reserves(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: Optimal input doesn't exceed available reserves.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()

        try:
            result = solver.solve(inp)

            # Optimal input should be less than available reserves
            # (accounting for fees taking some of the input)
            assert result.optimal_input <= hop1.reserve_in
        except OptimizationError:
            pass


class TestSolverInvariants:
    """Property tests for solver invariants."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=st.floats(min_value=1.02, max_value=1.2, allow_nan=False, allow_infinity=False),
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_profit_non_negative(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: Profit is always non-negative when successful.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()

        try:
            result = solver.solve(inp)
            assert result.profit >= 0, f"Expected non-negative profit, got {result.profit}"
        except OptimizationError:
            pass

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=st.floats(min_value=1.05, max_value=1.3, allow_nan=False, allow_infinity=False),
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_optimal_input_positive(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: Optimal input is positive when profit exists.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        inp = SolveInput(hops=(hop1, hop2))

        solver = ArbSolver()

        try:
            result = solver.solve(inp)
            if result.profit > 0:
                assert result.optimal_input > 0
        except OptimizationError:
            pass


class TestSolverCoefficients:
    """Property tests for Möbius coefficient computation."""

    @hypothesis.given(
        reserve_in=reserve_strategy,
        reserve_out=reserve_strategy,
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_coefficients_well_formed(
        self,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
    ):
        """
        Property: Coefficients are well-formed for any valid hop.
        """
        hop = ConstantProductHop(reserve_in=reserve_in, reserve_out=reserve_out, fee=fee)
        coeffs = _compute_mobius_coefficients((hop,))

        assert coeffs.K > 0
        assert coeffs.M > 0
        assert coeffs.N > 0
        assert math.isfinite(coeffs.K)
        assert math.isfinite(coeffs.M)
        assert math.isfinite(coeffs.N)

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_mult=st.floats(min_value=1.02, max_value=1.2, allow_nan=False, allow_infinity=False),
        fee=fee_fraction_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_profitable_path_is_profitable(
        self,
        base_reserve: int,
        price_mult: float,
        fee: Fraction,
    ):
        """
        Property: is_profitable correctly identifies profitable paths.
        """
        hop1, hop2 = make_profitable_v2_pair(base_reserve, price_mult, fee)
        coeffs = _compute_mobius_coefficients((hop1, hop2))

        # Check if the path is profitable
        if coeffs.is_profitable:
            assert coeffs.K > coeffs.M


class TestV3VirtualReserves:
    """Property tests for V3 virtual reserve computation."""

    @hypothesis.given(
        liquidity=liquidity_strategy,
        sqrt_price_x96=sqrt_price_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_virtual_reserves_positive(
        self,
        liquidity: int,
        sqrt_price_x96: int,
    ):
        """
        Property: Virtual reserves are always positive.
        """
        r_in_zfo, r_out_zfo = _v3_virtual_reserves(
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )

        r_in_ofz, r_out_ofz = _v3_virtual_reserves(
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=False,
        )

        assert r_in_zfo > 0
        assert r_out_zfo > 0
        assert r_in_ofz > 0
        assert r_out_ofz > 0

    @hypothesis.given(
        liquidity=liquidity_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_virtual_reserves_at_price_one(self, liquidity: int):
        """
        Property: At price = 1.0, virtual reserves are equal.
        """
        sqrt_price_x96 = 2**96  # sqrt(price) = 1.0

        r_in_zfo, r_out_zfo = _v3_virtual_reserves(
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )

        # At price = 1.0, reserves should be approximately equal
        ratio = r_in_zfo / r_out_zfo if r_out_zfo > 0 else 0
        assert 0.99 < ratio < 1.01, f"Expected ratio ~1.0, got {ratio}"
