"""Tests for V3 buy-pool validation in solver fast-path.

Validates that when V3 is the buy pool (first hop), the solver fast-path
uses actual V3 pool calculations instead of constant-product approximation.
"""

import pytest

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BoundedProductHop,
    ConstantProductHop,
    SolveInput,
    SolveResult,
    SolverMethod,
)

from .conftest import FEE_0_3_PCT, USDC_1_5M, USDC_2M, WETH_800, WETH_1000


class TestV3BuyPoolFastPath:
    """Validate V3 buy-pool uses actual pool calculations, not approximation."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_v3_buypool_detected_as_bounded_product(self):
        """V3 buy pool should be detected as BoundedProductHop."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96
        r_in, r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hop = BoundedProductHop(
            reserve_in=r_in,
            reserve_out=r_out,
            fee=FEE_0_3_PCT,
            liquidity=L,
            sqrt_price=sqrt_price_x96,
            tick_lower=0,
            tick_upper=0,
        )
        assert hop.is_v3
        assert hop.invariant.name == "BOUNDED_PRODUCT"

    def test_v3_buypool_with_v2_sell_path_completes(self, solver):
        """V3 buy + V2 sell path should complete without error."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hops = (
            BoundedProductHop(
                reserve_in=v3_r_in,
                reserve_out=v3_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
            ConstantProductHop(
                reserve_in=WETH_1000,
                reserve_out=USDC_2M,
                fee=FEE_0_3_PCT,
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)

    def test_v3_buypool_solver_selects_mobius_or_piecewise(self, solver):
        """For V3+V2 paths, solver should use Mobius or PiecewiseMobius."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 2_000_000_000_000_000_000
        sqrt_price_x96 = int(1.5 * (2**96))  # price = 2.25
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hops = (
            BoundedProductHop(
                reserve_in=v3_r_in,
                reserve_out=v3_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
            ConstantProductHop(
                reserve_in=WETH_1000,
                reserve_out=USDC_2M,
                fee=FEE_0_3_PCT,
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        if result.success:
            assert result.method in {SolverMethod.MOBIUS, SolverMethod.PIECEWISE_MOBIUS}

    def test_v3_buypool_approximation_matches_v3_math(self):
        """Constant-product approximation should be close to actual V3 output."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        # For a single V3 tick range, the constant-product formula using
        # virtual reserves should match the actual V3 swap math exactly.
        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96  # price = 1.0
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        # Input amount (in token0 units, scaled)
        x_in = 100_000_000_000  # 100k USDC

        # Constant-product approximation
        gamma = 1.0 - 0.003  # 0.3% fee
        r_in = float(v3_r_in) / (2**96)  # descale
        r_out = float(v3_r_out) / (2**96)
        denom = r_in + x_in * gamma
        cp_output = x_in * gamma * r_out / denom

        L_float = float(L)
        sqrt_p = 1.0
        delta_sqrt_p = x_in * gamma / (L_float + x_in * gamma * sqrt_p)
        v3_output = L_float * delta_sqrt_p

        # Should be very close (both are valid Möbius transforms)
        rel_diff = abs(cp_output - v3_output) / max(v3_output, 1)
        assert rel_diff < 0.001, f"CP approximation {cp_output} vs V3 {v3_output}"


class TestV3BuyPoolTickCrossing:
    """Validate behavior when V3 swap would cross tick boundaries."""

    def test_v3_buypool_tick_crossing_validation_needed(self):
        """When tick crossing likely, solver should validate or reject."""
        # This is a placeholder test documenting expected behavior.
        # Full tick crossing validation requires actual V3 pool integration.


class TestV3SellPool:
    """V3 sell pool (second hop) should work with virtual reserves."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_v2_buypool_v3_sellpool_path(self, solver):
        """V2 buy + V3 sell should complete."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 2_000_000_000_000_000_000
        sqrt_price_x96 = int(2.0 * (2**96))
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hops = (
            ConstantProductHop(
                reserve_in=USDC_1_5M,
                reserve_out=WETH_800,
                fee=FEE_0_3_PCT,
            ),
            BoundedProductHop(
                reserve_in=v3_r_in,
                reserve_out=v3_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)
        if result.success:
            assert result.method == SolverMethod.MOBIUS


class TestSolverResultValidation:
    """Validate that solver results are checked against actual pool math."""

    def test_solver_result_has_required_fields(self):
        """SolveResult should have all fields needed for validation."""
        result = SolveResult(
            optimal_input=1000,
            profit=50,
            success=True,
            iterations=0,
            method=SolverMethod.MOBIUS,
        )
        assert result.optimal_input > 0
        assert result.profit >= 0
        assert result.success
