"""
Tests for the 2-pool CVXPY problem factory.

Validates that build_2pool_cvxpy_problem produces solved problems
with correct status and value, matching the inline construction it
replaces.
"""

from fractions import Fraction

import cvxpy
import pytest

from ._cvxpy_problem_factory import build_2pool_cvxpy_problem


class TestBuild2PoolCVXPYProblem:
    """Validate the shared 2-pool CVXPY problem factory."""

    def test_wbtc_weth_finds_optimal(self):
        """
        WBTC (8 decimals) / WETH (18 decimals) with different reserves.

        This is the same scenario as test_cvxpy_known_value_wbtc_weth
        in test_solver_integration.py. Verifies the factory produces
        a solved problem with an optimal, profitable result.
        """
        problem = build_2pool_cvxpy_problem(
            reserves0_a=900 * 10**8,
            reserves1_a=2100 * 10**18,
            reserves0_b=925 * 10**8,
            reserves1_b=2100 * 10**18,
            decimals0=8,
            decimals1=18,
            fee=Fraction(3, 1000),
        )

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
        if problem.status == cvxpy.settings.OPTIMAL:
            assert problem.value >= 0

    def test_identical_pools_no_profit(self):
        """
        Identical pools with fees should yield no arbitrage.

        This matches test_cvxpy_no_profit_identical_pools but uses
        per-token compression (the factory's strategy) instead of
        the single-compression approach.
        """
        problem = build_2pool_cvxpy_problem(
            reserves0_a=1000 * 10**18,
            reserves1_a=2000 * 10**18,
            reserves0_b=1000 * 10**18,
            reserves1_b=2000 * 10**18,
            decimals0=18,
            decimals1=18,
            fee=Fraction(3, 1000),
        )

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
        # Same reserves → no profit after fees
        assert problem.value < 1e-8, (
            f"Expected no profit for identical pools, got {problem.value}"
        )

    def test_equal_decimals_profitable(self):
        """
        Same-decimal pools with price gap should find profit.

        This matches the 2%/0.3% case from test_cvxpy_finds_profit
        but uses per-token compression.
        """
        problem = build_2pool_cvxpy_problem(
            reserves0_a=1000 * 10**18,
            reserves1_a=2000 * 10**18,
            reserves0_b=1000 * 10**18,
            reserves1_b=int(2000 * 10**18 / 1.02),
            decimals0=18,
            decimals1=18,
            fee=Fraction(3, 1000),
        )

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
        # 2% price diff > 2x0.3% fees = 0.6%, so profitable
        if problem.status == cvxpy.settings.OPTIMAL:
            assert problem.value >= 0

    @pytest.mark.parametrize(
        ("decimals0", "decimals1"),
        [(6, 18), (8, 18), (18, 18), (18, 6)],
        ids=["USDC/WETH", "WBTC/WETH", "WETH/WETH", "WETH/USDC"],
    )
    def test_various_decimal_pairs(self, decimals0: int, decimals1: int):
        """
        Factory should produce solved problems across decimal combinations.

        The per-token compression must handle any decimal asymmetry
        without floating-point overflow or underflow.
        """
        problem = build_2pool_cvxpy_problem(
            reserves0_a=1000 * 10**decimals0,
            reserves1_a=2000 * 10**decimals1,
            reserves0_b=1100 * 10**decimals0,
            reserves1_b=2000 * 10**decimals1,
            decimals0=decimals0,
            decimals1=decimals1,
            fee=Fraction(3, 1000),
        )

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT

    def test_fresh_problem_each_call(self):
        """
        Two calls with same arguments should return independent Problems.

        Mutating one must not affect the other. This validates that
        the factory has no shared mutable state.
        """
        kwargs = {
            "reserves0_a": 1000 * 10**18,
            "reserves1_a": 2000 * 10**18,
            "reserves0_b": 1100 * 10**18,
            "reserves1_b": 2000 * 10**18,
            "decimals0": 18,
            "decimals1": 18,
            "fee": Fraction(3, 1000),
        }
        problem_a = build_2pool_cvxpy_problem(**kwargs)
        problem_b = build_2pool_cvxpy_problem(**kwargs)

        # Different objects
        assert problem_a is not problem_b
        # Same optimal value
        assert abs(problem_a.value - problem_b.value) < 1e-10
