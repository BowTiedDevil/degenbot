"""
Tests for 2-pool CVXPY DPP compliance and re-solve behavior.

Validates that _build_convex_problem produces a DPP-compliant problem
before the warm-up solve, and that re-solving with enforce_dpp=True
and updated parameters produces correct results.
"""


import cvxpy
import numpy as np
from cvxpy.atoms.geo_mean import geo_mean

from degenbot.arbitrage._legacy._uniswap_2pool_cycle_testing import _build_convex_problem


class TestTwoPoolDPPCompliance:
    """Validate DPP compliance of the pre-built 2-pool CVXPY problem."""

    def test_problem_is_dcp_dpp_before_solve(self):
        """
        The 2-pool problem should be DPP-compliant before any solve call.

        This catches non-DPP construction at import time, before the warm-up
        solve populates internal state and the Problem is pickled into worker
        processes.
        """
        problem = _build_convex_problem(num_pools=2)
        assert problem.is_dcp(dpp=True), (
            "2-pool CVXPY problem is not DPP-compliant. "
            "This will break DPP re-canonicalization in worker processes."
        )

    def test_reuse_with_updated_parameters(self):
        """
        Re-solving with updated parameters and enforce_dpp=True should produce
        a positive result for a known-profitable pool pair.

        This verifies the full lifecycle: pre-build → pickle → update → re-solve.
        """
        problem = _build_convex_problem(num_pools=2)

        # Set up a known-profitable pool pair:
        # Pool A: 900 WBTC / 2100 WETH (WBTC expensive)
        # Pool B: 925 WBTC / 2100 WETH (WBTC cheap)
        # Profit direction: buy WBTC in pool B, sell in pool A
        reserves_a_0 = 900.0
        reserves_a_1 = 2100.0
        reserves_b_0 = 925.0
        reserves_b_1 = 2100.0
        fee = 0.003

        compressed_reserves = problem.param_dict["compressed_reserves_pre_swap"]
        pool_hi_k = problem.param_dict["pool_hi_pre_swap_k"]
        pool_lo_k = problem.param_dict["pool_lo_pre_swap_k"]
        fee_multiplier = problem.param_dict["fee_multiplier"]

        # Update reserves (compressed to [0,1] range)
        max_r0 = max(reserves_a_0, reserves_b_0)
        max_r1 = max(reserves_a_1, reserves_b_1)
        compressed_reserves.save_value(
            np.array(
                (
                    (reserves_a_0 / max_r0, reserves_a_1 / max_r1),
                    (reserves_b_0 / max_r0, reserves_b_1 / max_r1),
                ),
                dtype=np.float64,
            )
        )

        # Update k values (computed from the parameter's current value)
        pool_hi_k.save_value(
            geo_mean(compressed_reserves[0]).value
        )
        pool_lo_k.save_value(
            geo_mean(compressed_reserves[1]).value
        )

        # Update fees
        fee_multiplier.save_value(
            np.array(
                ((1 - fee, 1 - fee), (1 - fee, 1 - fee)),
                dtype=np.float64,
            )
        )

        # Re-solve with enforce_dpp=True
        problem.solve(solver=cvxpy.CLARABEL, enforce_dpp=True)

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT, (
            f"Re-solve failed with status: {problem.status}"
        )
        assert problem.value > 0, (
            f"Expected positive profit for known-profitable pools, got {problem.value}"
        )

    def test_reuse_no_profit_identical_pools(self):
        """
        Re-solving with identical pools should yield no profit after fees.

        This validates the re-solve path produces correct results for the
        unprofitable case too.
        """
        problem = _build_convex_problem(num_pools=2)

        # Identical pools — no arbitrage possible after fees
        reserves_0 = 1000.0
        reserves_1 = 2000.0
        fee = 0.003

        compressed_reserves = problem.param_dict["compressed_reserves_pre_swap"]
        pool_hi_k = problem.param_dict["pool_hi_pre_swap_k"]
        pool_lo_k = problem.param_dict["pool_lo_pre_swap_k"]
        fee_multiplier = problem.param_dict["fee_multiplier"]

        compressed_reserves.save_value(
            np.array(
                ((reserves_0, reserves_1), (reserves_0, reserves_1)),
                dtype=np.float64,
            )
        )
        pool_hi_k.save_value(
            geo_mean(compressed_reserves[0]).value
        )
        pool_lo_k.save_value(
            geo_mean(compressed_reserves[1]).value
        )
        fee_multiplier.save_value(
            np.array(
                ((1 - fee, 1 - fee), (1 - fee, 1 - fee)),
                dtype=np.float64,
            )
        )

        problem.solve(solver=cvxpy.CLARABEL, enforce_dpp=True)

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
        assert problem.value < 1e-8, (
            f"Expected no profit for identical pools with fees, got {problem.value}"
        )
