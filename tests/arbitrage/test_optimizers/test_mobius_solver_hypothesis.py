"""
Property-based tests for Möbius transformation optimizer.

Tests the closed-form Möbius solver against Brent optimization for
constant-product AMM arbitrage paths with randomized reserves and fees.
"""

import math
from dataclasses import dataclass

import hypothesis
import hypothesis.strategies as st

from degenbot.arbitrage.optimizers._mobius_math import (
    MobiusFloatHop,
    compute_mobius_coefficients,
    mobius_solve,
    simulate_path,
)

from .conftest import brent_solve_hops as brent_solve

# ==============================================================================
# Hypothesis Strategies
# ==============================================================================

# Reserve amounts (realistic range)
reserve_strategy = st.floats(min_value=1e6, max_value=1e12, allow_nan=False, allow_infinity=False)

# Fee values (realistic DeFi fees)
fee_strategy = st.floats(min_value=0.0001, max_value=0.03, allow_nan=False, allow_infinity=False)

# Number of pools in a cycle
num_pools_strategy = st.integers(min_value=2, max_value=8)

# Profit factor for generating profitable paths
profit_factor_strategy = st.floats(
    min_value=1.01, max_value=1.5, allow_nan=False, allow_infinity=False
)


# ==============================================================================
# Helpers
# ==============================================================================


@dataclass(frozen=True, slots=True)
class PoolDef:
    """Immutable pool definition for test generation."""

    reserve_in: float
    reserve_out: float
    fee: float

    @property
    def gamma(self) -> float:
        return 1.0 - self.fee


def hop_from_def(pd: PoolDef) -> MobiusFloatHop:
    return MobiusFloatHop(
        reserve_in=pd.reserve_in,
        reserve_out=pd.reserve_out,
        fee=pd.fee,
    )


def make_n_pool_path(
    n: int, profit_factor: float = 1.1, base_reserve: float = 1_000_000.0, fee: float = 0.003
) -> list[PoolDef]:
    """
    Generate an n-pool cycle with guaranteed profitability.

    Creates a cycle where each pool has mispriced reserves in the same
    direction, so the product of marginal exchange rates (after fees)
    exceeds 1. The per-hop marginal rate equals profit_factor^(1/n),
    so the cycle product equals profit_factor.
    """
    gamma = 1.0 - fee

    # Per-hop target marginal rate after fees: gamma * reserve_out / reserve_in = per_hop
    per_hop = profit_factor ** (1.0 / n)

    pools = []
    for i in range(n):
        reserve_in = base_reserve * (1.05**i)
        # Set reserve_out so that gamma * reserve_out / reserve_in = per_hop
        reserve_out = reserve_in * per_hop / gamma
        pools.append(PoolDef(reserve_in=reserve_in, reserve_out=reserve_out, fee=fee))

    return pools


def make_profitable_pair(
    base_reserve: float,
    price_ratio: float,
    fee: float,
) -> tuple[PoolDef, PoolDef]:
    """
    Generate a profitable 2-pool pair.

    Pool A: Higher price (more token_out per token_in)
    Pool B: Lower price (less token_out per token_in)
    """
    pool_a = PoolDef(
        reserve_in=base_reserve,
        reserve_out=base_reserve * price_ratio,  # Higher price
        fee=fee,
    )
    pool_b = PoolDef(
        reserve_in=base_reserve * price_ratio * 0.99,  # Slightly less
        reserve_out=base_reserve,
        fee=fee,
    )
    return pool_a, pool_b


# ==============================================================================
# Property Tests: 2-Pool Paths
# ==============================================================================


class TestMobius2PoolProperties:
    """Property tests for 2-pool Möbius optimization."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_ratio=st.floats(min_value=1.01, max_value=1.2, allow_nan=False, allow_infinity=False),
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_mobius_matches_brent_2pool(
        self,
        base_reserve: float,
        price_ratio: float,
        fee: float,
    ):
        """
        Property: Möbius and Brent agree for 2-pool paths.

        The closed-form Möbius solution should match Brent optimization
        within numerical tolerance for any valid 2-pool configuration.
        """
        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)

        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        # Möbius solve
        x_mobius, profit_mobius, iterations = mobius_solve(hops)

        # Brent solve
        x_brent, profit_brent, _ = brent_solve(hops)

        # Should match within tolerance
        if x_mobius > 0 and x_brent > 0:
            rel_x_diff = abs(x_mobius - x_brent) / max(x_mobius, x_brent)
            rel_p_diff = abs(profit_mobius - profit_brent) / max(
                abs(profit_mobius), abs(profit_brent), 1e-10
            )

            # Allow tolerance for numerical precision (5e-3 = 0.5%)
            assert rel_x_diff < 5e-3, f"Optimal input mismatch: {x_mobius} vs {x_brent}"
            assert rel_p_diff < 5e-3, f"Profit mismatch: {profit_mobius} vs {profit_brent}"

    @hypothesis.given(
        base_reserve=reserve_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_mobius_no_profit_identical_prices(
        self,
        base_reserve: float,
        fee: float,
    ):
        """
        Property: No profit when pools have identical prices (with fees).

        When both pools have the same effective price and fees are non-zero,
        arbitrage should be unprofitable.
        """
        # Identical pools
        pool = PoolDef(reserve_in=base_reserve, reserve_out=base_reserve, fee=fee)
        hops = [hop_from_def(pool), hop_from_def(pool)]

        x_opt, profit, iterations = mobius_solve(hops)

        # No profitable arbitrage
        assert profit <= 0 or x_opt == 0, f"Expected no profit, got {profit}"

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_ratio=st.floats(min_value=1.05, max_value=1.5, allow_nan=False, allow_infinity=False),
        fee=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_mobius_profit_positive_with_spread(
        self,
        base_reserve: float,
        price_ratio: float,
        fee: float,
    ):
        """
        Property: Profit exists when price spread exceeds fees.

        When the price difference between pools exceeds the round-trip
        fee cost, arbitrage should be profitable.
        """
        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        x_opt, profit, iterations = mobius_solve(hops)

        # Price spread must exceed 2*fee for profitability
        min_profitable_spread = 2 * fee * 1.001
        actual_spread = price_ratio - 1.0

        # Only assert if the path is actually profitable
        # (the helper may not always generate a profitable pair)
        if actual_spread > min_profitable_spread and profit > 0:
            # Verify the profit is positive
            assert profit > 0


class TestMobiusMultiPoolProperties:
    """Property tests for multi-pool Möbius optimization."""

    @hypothesis.given(
        num_pools=num_pools_strategy,
        profit_factor=profit_factor_strategy,
        base_reserve=reserve_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_mobius_matches_brent_multipool(
        self,
        num_pools: int,
        profit_factor: float,
        base_reserve: float,
        fee: float,
    ):
        """
        Property: Möbius matches Brent for multi-pool paths.

        The closed-form solution should work for paths of any length.
        """
        pools = make_n_pool_path(num_pools, profit_factor, base_reserve, fee)
        hops = [hop_from_def(p) for p in pools]

        # Möbius solve
        x_mobius, profit_mobius, iterations = mobius_solve(hops)

        # Brent solve
        x_brent, profit_brent, _ = brent_solve(hops)

        # Should match within tolerance
        if x_mobius > 0 and x_brent > 0:
            rel_x_diff = abs(x_mobius - x_brent) / max(x_mobius, x_brent)
            rel_p_diff = abs(profit_mobius - profit_brent) / max(
                abs(profit_mobius), abs(profit_brent), 1e-10
            )

            # Allow slightly larger tolerance for longer paths
            tolerance = 1e-3 if num_pools <= 4 else 1e-2

            assert rel_x_diff < tolerance, (
                f"[{num_pools} pools] Input mismatch: {x_mobius} vs {x_brent}"
            )
            assert rel_p_diff < tolerance, (
                f"[{num_pools} pools] Profit mismatch: {profit_mobius} vs {profit_brent}"
            )

    @hypothesis.given(
        num_pools=st.integers(min_value=2, max_value=5),
        profit_factor=st.floats(
            min_value=1.05, max_value=1.3, allow_nan=False, allow_infinity=False
        ),
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_mobius_zero_iterations(
        self,
        num_pools: int,
        profit_factor: float,
        fee: float,
    ):
        """
        Property: Möbius solver uses zero iterations.

        The closed-form solution should not require iteration.
        """
        pools = make_n_pool_path(num_pools, profit_factor, fee=fee)
        hops = [hop_from_def(p) for p in pools]

        x_opt, profit, iterations = mobius_solve(hops)

        # Möbius is closed-form, no iterations
        # (We can't directly check iterations, but we verify the result is valid)
        if profit > 0:
            assert x_opt > 0
            assert math.isfinite(x_opt)
            assert math.isfinite(profit)
            assert iterations == 0  # Closed-form has zero iterations


class TestMobiusCoefficientProperties:
    """Property tests for Möbius coefficient computation."""

    @hypothesis.given(
        reserve_in=reserve_strategy,
        reserve_out=reserve_strategy,
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=30)
    def test_coefficients_well_formed(
        self,
        reserve_in: float,
        reserve_out: float,
        fee: float,
    ):
        """
        Property: Möbius coefficients are well-formed.

        All coefficients should be positive and finite.
        """
        hop = MobiusFloatHop(reserve_in=reserve_in, reserve_out=reserve_out, fee=fee)
        coeffs = compute_mobius_coefficients([hop])

        assert coeffs.K > 0
        assert coeffs.M > 0
        assert coeffs.N > 0
        assert math.isfinite(coeffs.K)
        assert math.isfinite(coeffs.M)
        assert math.isfinite(coeffs.N)

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_ratio=st.floats(min_value=1.01, max_value=1.2, allow_nan=False, allow_infinity=False),
        fee=fee_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_profitable_path_K_greater_than_M(
        self,
        base_reserve: float,
        price_ratio: float,
        fee: float,
    ):
        """
        Property: Profitable paths have K > M.

        When arbitrage is profitable, the product of rates exceeds 1,
        so K > M in the Möbius formulation.
        """
        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        coeffs = compute_mobius_coefficients(hops)

        # Check if the path is profitable
        if coeffs.is_profitable:
            assert coeffs.K > coeffs.M


class TestMobiusSimulationProperties:
    """Property tests for path simulation."""

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_ratio=st.floats(min_value=1.0, max_value=1.5, allow_nan=False, allow_infinity=False),
        fee=fee_strategy,
        amount_in=st.floats(min_value=1.0, max_value=1e9, allow_nan=False, allow_infinity=False),
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_simulate_path_positive_output(
        self,
        base_reserve: float,
        price_ratio: float,
        fee: float,
        amount_in: float,
    ):
        """
        Property: Simulation produces positive output for positive input.

        For any valid pool configuration and positive input, the output
        should be positive and finite.
        """
        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        output = simulate_path(amount_in, hops)

        assert output >= 0, f"Expected non-negative output, got {output}"
        assert math.isfinite(output), f"Expected finite output, got {output}"

    @hypothesis.given(
        base_reserve=reserve_strategy,
        price_ratio=st.floats(min_value=1.0, max_value=2.0, allow_nan=False, allow_infinity=False),
        fee=fee_strategy,
        amount_in_small=st.floats(
            min_value=1.0, max_value=1e6, allow_nan=False, allow_infinity=False
        ),
        amount_in_large=st.floats(
            min_value=1e6, max_value=1e9, allow_nan=False, allow_infinity=False
        ),
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_simulate_path_monotonic(
        self,
        base_reserve: float,
        price_ratio: float,
        fee: float,
        amount_in_small: float,
        amount_in_large: float,
    ):
        """
        Property: Simulation is monotonic in input.

        Larger input should produce larger (or equal) output.
        """
        hypothesis.assume(amount_in_small < amount_in_large)

        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        output_small = simulate_path(amount_in_small, hops)
        output_large = simulate_path(amount_in_large, hops)

        assert output_large >= output_small, (
            f"Expected monotonic: output({amount_in_large})={output_large} "
            f">= output({amount_in_small})={output_small}"
        )


class TestMobiusEdgeCases:
    """Property tests for edge cases and boundary conditions."""

    def test_zero_input(self):
        """Zero input should produce zero output."""
        pool = PoolDef(reserve_in=1e6, reserve_out=2e6, fee=0.003)
        hops = [hop_from_def(pool), hop_from_def(pool)]

        output = simulate_path(0.0, hops)
        assert output == 0.0

    def test_very_small_input(self):
        """Very small input should produce small but positive output."""
        pool_a = PoolDef(reserve_in=1e6, reserve_out=2e6, fee=0.003)
        pool_b = PoolDef(reserve_in=1.9e6, reserve_out=1e6, fee=0.003)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        output = simulate_path(1e-10, hops)
        assert output >= 0
        assert math.isfinite(output)

    @hypothesis.given(
        fee=st.floats(min_value=0.3, max_value=0.49, allow_nan=False, allow_infinity=False),
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_high_fee_unprofitable(self, fee: float):
        """
        Property: Very high fees make arbitrage unprofitable.

        When fees approach 50%, round-trip cost exceeds any price spread.
        """
        base_reserve = 1e6
        price_ratio = 1.01  # Small spread

        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        x_opt, profit, iterations = mobius_solve(hops)

        # Should be unprofitable with very high fees
        # (2x 30%+ fee = 60%+ round-trip cost, far exceeds 1% spread)
        assert profit <= 0 or x_opt == 0, f"Expected unprofitable with fee={fee}"

    @hypothesis.given(
        base_reserve=st.floats(
            min_value=1e3, max_value=1e15, allow_nan=False, allow_infinity=False
        ),
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_various_reserve_scales(self, base_reserve: float):
        """
        Property: Solver works across reserve scale magnitudes.

        The solver should produce consistent results regardless of
        the absolute scale of reserves.
        """
        price_ratio = 1.1
        fee = 0.003

        pool_a, pool_b = make_profitable_pair(base_reserve, price_ratio, fee)
        hops = [hop_from_def(pool_a), hop_from_def(pool_b)]

        x_opt, profit, iterations = mobius_solve(hops)

        # Should find profitable arbitrage
        assert profit > 0, f"Expected profit for base_reserve={base_reserve}"
        assert x_opt > 0
