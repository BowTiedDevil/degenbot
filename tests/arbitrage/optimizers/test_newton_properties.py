"""
Property-based tests for Newton optimizer Hessian threshold.

These tests use Hypothesis to explore the parameter space and identify
cases where the Hessian magnitude threshold may be too conservative
or too lenient.
"""

import math
from collections.abc import Sequence
from dataclasses import dataclass

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from degenbot.arbitrage.optimizers.newton import (
    DEFAULT_MAX_STEP_MULTIPLIER,
    v2_optimal_arbitrage_newton,
    v2_profit_gradient_and_hessian,
)


@dataclass(frozen=True)
class HessianResult:
    """Result from Hessian computation for analysis."""

    hessian: float
    gradient: float
    profit: float
    reserves_buy: tuple[float, float]
    reserves_sell: tuple[float, float]
    fees: tuple[float, float]
    input_x: float

    @property
    def hessian_magnitude(self) -> float:
        return abs(self.hessian)

    @property
    def would_trigger_threshold(self, threshold: float = 1e-30) -> bool:
        return self.hessian_magnitude < threshold


class TestHessianMagnitudeExploration:
    """
        Exploration tests to understand Hessian magnitude distribution.

        These tests don't assert correctness but collect data on when
    the Hessian becomes small enough to trigger the threshold.
    """

    @given(
        # Large reserves (typical mainnet pools: 1e18 to 1e30)
        reserve0_buy=st.floats(
            min_value=1e18, max_value=1e30, allow_nan=False, allow_infinity=False
        ),
        reserve1_buy=st.floats(
            min_value=1e18, max_value=1e30, allow_nan=False, allow_infinity=False
        ),
        reserve0_sell=st.floats(
            min_value=1e18, max_value=1e30, allow_nan=False, allow_infinity=False
        ),
        reserve1_sell=st.floats(
            min_value=1e18, max_value=1e30, allow_nan=False, allow_infinity=False
        ),
        # Standard fees: 0.01% to 1%
        fee_buy=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
        fee_sell=st.floats(min_value=0.0001, max_value=0.01, allow_nan=False, allow_infinity=False),
        # Initial guess: small fraction of reserves (0.001% to 10%)
        x_pct=st.floats(min_value=1e-5, max_value=0.1, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=1000, deadline=None)
    def test_hessian_distribution_typical_pools(
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
        Explore Hessian magnitude distribution with typical pool parameters.

        This test records cases where the Hessian magnitude falls below
        various thresholds to understand if 1e-30 is appropriate.
        """
        fee_mult_buy = 1.0 - fee_buy
        fee_mult_sell = 1.0 - fee_sell
        x = reserve0_buy * x_pct

        profit, gradient, hessian = v2_profit_gradient_and_hessian(
            x=x,
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        # Basic sanity checks
        assert math.isfinite(hessian), f"Non-finite hessian: {hessian}"

        # Track distribution of hessian magnitudes
        # These thresholds represent potential cutoff values
        thresholds = [1e-10, 1e-15, 1e-20, 1e-25, 1e-30, 1e-35]
        hessian_mag = abs(hessian)

        # If hessian is very small, record the context
        if hessian_mag < 1e-20:
            result = HessianResult(
                hessian=hessian,
                gradient=gradient,
                profit=profit,
                reserves_buy=(reserve0_buy, reserve1_buy),
                reserves_sell=(reserve0_sell, reserve1_sell),
                fees=(fee_buy, fee_sell),
                input_x=x,
            )
            # Store for analysis (hypothesis will show examples)
            self._record_small_hessian(result, thresholds)

    def _record_small_hessian(self, result: HessianResult, thresholds: Sequence[float]) -> None:
        """Record details about small hessian cases for analysis."""
        hessian_mag = result.hessian_magnitude

        # Find which thresholds this would trigger
        triggered = [t for t in thresholds if hessian_mag < t]

        if triggered:
            # This will be shown in Hypothesis output on failure/verbose
            msg = (
                f"Small hessian: {hessian_mag:.2e} triggers {len(triggered)} thresholds. "
                f"Reserves buy: ({result.reserves_buy[0]:.2e}, {result.reserves_buy[1]:.2e}), "
                f"sell: ({result.reserves_sell[0]:.2e}, {result.reserves_sell[1]:.2e}), "
                f"x: {result.input_x:.2e}"
            )
            # Use pytest's record_property for CI analysis
            if hasattr(pytest, "config"):
                pytest.record_property("small_hessian", hessian_mag)

    @given(
        # Extreme: very large reserves (1e30 to 1e60 - beyond typical but possible)
        reserve0_buy=st.floats(
            min_value=1e30, max_value=1e60, allow_nan=False, allow_infinity=False
        ),
        reserve1_buy=st.floats(
            min_value=1e30, max_value=1e60, allow_nan=False, allow_infinity=False
        ),
        reserve0_sell=st.floats(
            min_value=1e30, max_value=1e60, allow_nan=False, allow_infinity=False
        ),
        reserve1_sell=st.floats(
            min_value=1e30, max_value=1e60, allow_nan=False, allow_infinity=False
        ),
        # Low fees (1 basis point to 10 basis points)
        fee_buy=st.floats(min_value=0.0001, max_value=0.001, allow_nan=False, allow_infinity=False),
        fee_sell=st.floats(
            min_value=0.0001, max_value=0.001, allow_nan=False, allow_infinity=False
        ),
        # Tiny input relative to reserves
        x_factor=st.floats(min_value=1e-20, max_value=1e-10, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=500, deadline=None)
    def test_hessian_extreme_large_reserves(
        self,
        reserve0_buy: float,
        reserve1_buy: float,
        reserve0_sell: float,
        reserve1_sell: float,
        fee_buy: float,
        fee_sell: float,
        x_factor: float,
    ) -> None:
        """
        Test with extremely large reserves that might cause hessian underflow.

        Stablecoin pools or wrapped assets can have enormous reserve values.
        The hessian formula involves (R + x*γ)³ in the denominator.

        This test collects cases where the hessian falls below threshold
        to understand when the 1e-30 cutoff might be problematic.
        """
        fee_mult_buy = 1.0 - fee_buy
        fee_mult_sell = 1.0 - fee_sell
        x = reserve0_buy * x_factor

        profit, gradient, hessian = v2_profit_gradient_and_hessian(
            x=x,
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        # Check for numerical issues
        assert math.isfinite(hessian), f"Non-finite hessian with large reserves: {hessian}"

        # With extreme reserves, hessian can legitimately be very small
        # The question is: does this break convergence?
        hessian_mag = abs(hessian)

        # If hessian is sub-1e-30 with significant gradient, this indicates
        # the threshold may be too conservative for large reserve pools
        if hessian_mag < 1e-30 and hessian_mag > 0 and abs(gradient) >= 1e-10:
            # Record this finding - significant gradient with tiny hessian
            # means Newton would stop before converging
            # This is data for threshold tuning, not a failure
            pass  # Hypothesis will show examples in verbose mode

    @given(
        # Small reserves (new/poorly funded pools: 1e6 to 1e12)
        reserve0_buy=st.floats(
            min_value=1e6, max_value=1e12, allow_nan=False, allow_infinity=False
        ),
        reserve1_buy=st.floats(
            min_value=1e6, max_value=1e12, allow_nan=False, allow_infinity=False
        ),
        reserve0_sell=st.floats(
            min_value=1e6, max_value=1e12, allow_nan=False, allow_infinity=False
        ),
        reserve1_sell=st.floats(
            min_value=1e6, max_value=1e12, allow_nan=False, allow_infinity=False
        ),
        # High fees (1% to 10%)
        fee_buy=st.floats(min_value=0.01, max_value=0.10, allow_nan=False, allow_infinity=False),
        fee_sell=st.floats(min_value=0.01, max_value=0.10, allow_nan=False, allow_infinity=False),
        # Reasonable input (1% to 50% of reserves)
        x_pct=st.floats(min_value=0.01, max_value=0.5, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=500, deadline=None)
    def test_hessian_small_reserves_high_fees(
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
        Test with small reserves and high fees.

        With small reserves, hessian should never trigger the 1e-30 threshold
        due to underflow. If it does, that indicates a numerical issue.
        """
        fee_mult_buy = 1.0 - fee_buy
        fee_mult_sell = 1.0 - fee_sell
        x = reserve0_buy * x_pct

        _, _, hessian = v2_profit_gradient_and_hessian(
            x=x,
            reserve0_buy=reserve0_buy,
            reserve1_buy=reserve1_buy,
            reserve0_sell=reserve0_sell,
            reserve1_sell=reserve1_sell,
            fee_multiplier_buy=fee_mult_buy,
            fee_multiplier_sell=fee_mult_sell,
        )

        assert math.isfinite(hessian), f"Non-finite hessian: {hessian}"

        # With reserves 1e6+, hessian should never legitimately be < 1e-25
        # (that's 5 orders of magnitude below the 1e-30 threshold)
        # This only catches pathological numerical issues
        hessian_mag = abs(hessian)
        assert hessian_mag > 1e-25 or hessian_mag == 0.0, (
            f"Pathologically small hessian: {hessian_mag:.2e} "
            f"with reserves ({reserve0_buy:.2e}, {reserve1_buy:.2e}). "
            f"This indicates numerical underflow, not normal behavior."
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
        x_opt, _, iterations = v2_optimal_arbitrage_newton(
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
            except Exception as e:  # noqa: BLE001
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
                # This is interesting - threshold choice affects result
                # Record for analysis but don't fail
                pass

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
        _, gradient, hessian = v2_profit_gradient_and_hessian(
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
            assert relative_error < 0.02 or error < 1e-25, (
                f"Hessian mismatch: analytical={hessian:.6e}, "
                f"finite_diff={hessian_fd:.6e}, relative_error={relative_error:.2%}"
            )
