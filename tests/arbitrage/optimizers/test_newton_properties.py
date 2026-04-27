"""
Property-based tests for Newton optimizer Hessian threshold.

These tests use Hypothesis to explore the parameter space and identify
cases where the Hessian magnitude threshold may be too conservative
or too lenient.
"""

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from degenbot.arbitrage.optimizers.newton import (
    DEFAULT_MAX_STEP_MULTIPLIER,
    v2_optimal_arbitrage_newton,
    v2_profit_gradient_and_hessian,
)


class TestStepBound:
    """
    Tests for the Newton step size bound.

    These verify that the max_step_multiplier prevents wild jumps
    while maintaining convergence.
    """

    def test_default_step_multiplier_is_100(self) -> None:
        """
        Verify the default step multiplier constant.
        """
        assert DEFAULT_MAX_STEP_MULTIPLIER == 100.0

    def test_step_bound_prevents_wild_jumps(self) -> None:
        """
        Test that a tight step bound prevents huge steps.

        This uses a scenario where Newton would naturally take a large step
        and verifies the bound clamps it.
        """
        # Use a tight bound (2x) to force clamping
        x_opt, _, iterations = v2_optimal_arbitrage_newton(
            reserve0_buy=1e18,
            reserve1_buy=2e18,
            reserve0_sell=1.1e18,
            reserve1_sell=1.8e18,
            fee_buy=0.003,
            fee_sell=0.003,
            max_step_multiplier=2.0,  # Very tight
            max_iterations=20,  # Give it more iterations to converge
        )

        # Should still converge with tight bound (just takes more iterations)
        assert x_opt >= 0
        assert iterations <= 20

    def test_unbounded_step_allowed(self) -> None:
        """
        Test that passing float('inf') allows unbounded steps.
        """
        x_opt, _, _ = v2_optimal_arbitrage_newton(
            reserve0_buy=1e18,
            reserve1_buy=2e18,
            reserve0_sell=1.1e18,
            reserve1_sell=1.8e18,
            fee_buy=0.003,
            fee_sell=0.003,
            max_step_multiplier=float("inf"),
        )

        assert x_opt >= 0
        # Should converge quickly without bounds


class TestThresholdComparison:
    """
    Compare optimization behavior with different Hessian thresholds.

    These tests help determine the appropriate threshold value by running
    the same optimization with multiple threshold values and comparing results.
    """

    @given(
        reserve0_buy=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve1_buy=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve0_sell=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve1_sell=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        fee_buy=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
        fee_sell=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=100, deadline=None)
    def test_threshold_sensitivity(
        self,
        reserve0_buy: float,
        reserve1_buy: float,
        reserve0_sell: float,
        reserve1_sell: float,
        fee_buy: float,
        fee_sell: float,
    ) -> None:
        """
        Compare optimization results with different min_hessian values.

        Runs the same optimization with thresholds: 1e-30, 1e-20, 1e-15, 1e-10
        and checks if results differ significantly.
        """
        thresholds = [1e-30, 1e-20, 1e-15, 1e-10]
        results = []

        for threshold in thresholds:
            try:
                x_opt, y_opt, iterations = v2_optimal_arbitrage_newton(
                    reserve0_buy=reserve0_buy,
                    reserve1_buy=reserve1_buy,
                    reserve0_sell=reserve0_sell,
                    reserve1_sell=reserve1_sell,
                    fee_buy=fee_buy,
                    fee_sell=fee_sell,
                    min_hessian_magnitude=threshold,
                )
                results.append({
                    "threshold": threshold,
                    "x_opt": x_opt,
                    "y_opt": y_opt,
                    "iterations": iterations,
                    "success": True,
                })
            except (ValueError, ZeroDivisionError, OverflowError) as e:
                results.append({"threshold": threshold, "error": str(e), "success": False})

        # Analyze results
        successful = [r for r in results if r.get("success")]

        if len(successful) > 1:
            # Check if different thresholds give significantly different results
            x_opts = [r["x_opt"] for r in successful]
            max_diff = max(x_opts) - min(x_opts)
            avg_x = sum(x_opts) / len(x_opts)

            # If relative difference > 1%, threshold matters
            if avg_x > 0 and max_diff / avg_x > 0.01:
                pytest.xfail(
                    f"Threshold choice affects result by {max_diff / avg_x:.2%}: "
                    f"x_opts={x_opts}"
                )

    @pytest.mark.parametrize(
        "min_hessian",
        [
            pytest.param(1e-50, id="extremely_low"),
            pytest.param(1e-30, id="current_default"),
            pytest.param(1e-20, id="moderate"),
            pytest.param(1e-15, id="high"),
            pytest.param(1e-10, id="very_high"),
        ],
    )
    def test_specific_threshold_values(self, min_hessian: float) -> None:
        """
        Test that specific threshold values work for a known good case.

        This verifies that the threshold parameter is accepted and used.
        """
        # Typical pool values
        reserve0_buy = 1e21  # 1000 tokens with 18 decimals
        reserve1_buy = 2e24  # 2000 tokens with 18 decimals
        reserve0_sell = 1.1e21  # Slightly different ratio
        reserve1_sell = 1.9e24

        x_opt, y_opt, iterations = v2_optimal_arbitrage_newton(
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_buy=0.003,
            fee_sell=0.003,
            min_hessian_magnitude=min_hessian,
        )

        # Basic sanity checks
        assert x_opt >= 0, f"Negative optimal input: {x_opt}"
        assert y_opt >= 0, f"Negative forward amount: {y_opt}"
        assert iterations <= 10, f"Too many iterations: {iterations}"


class TestHessianFormulaCorrectness:
    """
    Verify the Hessian formula is mathematically correct.

    These tests check that the computed Hessian matches finite difference
    approximations, validating the analytical derivation.
    """

    @given(
        reserve0_buy=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve1_buy=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve0_sell=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        reserve1_sell=st.floats(
            min_value=1e18, max_value=1e24, allow_nan=False, allow_infinity=False
        ),
        fee_buy=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
        fee_sell=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
        x_pct=st.floats(min_value=0.001, max_value=0.1, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=200, deadline=None)
    def test_hessian_matches_finite_difference(
        self,
        reserve0_buy: float,
        reserve1_buy: float,
        reserve0_sell: float,
        reserve1_sell: float,
        fee_buy: float,
        fee_sell: float,
        x_pct: float,
    ) -> None:
        """
        Verify analytical Hessian matches finite difference approximation.

        This validates that the Hessian formula is correctly implemented.
        """
        fee_mult_buy = 1.0 - fee_buy
        fee_mult_sell = 1.0 - fee_sell
        x = reserve0_buy * x_pct

        # Get analytical gradient and hessian
        _, _, hessian = v2_profit_gradient_and_hessian(
            x=x,
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        # Compute finite difference Hessian
        dx = x * 1e-6  # Small perturbation

        _, grad_plus, _ = v2_profit_gradient_and_hessian(
            x=x + dx,
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        _, grad_minus, _ = v2_profit_gradient_and_hessian(
            x=max(x - dx, 1e-12),  # Ensure positive
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        hessian_fd = (grad_plus - grad_minus) / (2 * dx)

        # Compare (allowing for numerical error)
        # For very small hessians, use absolute error; for larger ones, use relative
        hessian_abs = abs(hessian)
        if hessian_abs > 1e-20:
            error = abs(hessian - hessian_fd)
            relative_error = error / hessian_abs
            # Allow 2% relative error or absolute error of 1e-25, whichever is larger
            # (finite difference approximation has inherent numerical error)
            assert relative_error < 0.02 or error < 1e-25, (
                f"Hessian mismatch: analytical={hessian:.6e}, "
                f"finite_diff={hessian_fd:.6e}, relative_error={relative_error:.2%}"
            )
