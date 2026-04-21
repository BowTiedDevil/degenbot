"""
Tests for the unified solver interface.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BrentSolver,
    Hop,
    MobiusSolver,
    NewtonSolver,
    SolveInput,
    SolveResult,
    SolverMethod,
    _compute_mobius_coefficients,
    _simulate_path,
)

from .conftest import (
    FEE_0_3_PCT,
    FEE_0_5_PCT,
    USDC_1_5M,
    USDC_2M,
    WETH_800,
    WETH_1000,
    make_2hop_v2_input,
)

# ---------------------------------------------------------------------------
# Test Core Types
# ---------------------------------------------------------------------------


class TestHop:
    def test_v2_hop(self):
        hop = Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert not hop.is_v3
        assert hop.gamma == pytest.approx(0.997)

    def test_v3_hop(self):
        hop = Hop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        assert hop.is_v3

    def test_v3_hop_partial_data_is_not_v3(self):
        """Only some V3 fields set → not a V3 hop."""
        hop = Hop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
        )
        assert not hop.is_v3

    def test_frozen(self):
        hop = Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        with pytest.raises(AttributeError):
            hop.reserve_in = 0


class TestSolveInput:
    def test_properties(self):
        inp = make_2hop_v2_input()
        assert inp.num_hops == 2
        assert inp.all_v2
        assert not inp.has_v3
        assert inp.v3_indices == ()

    def test_mixed_v2_v3(self):
        v2 = Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        v3 = Hop(
            reserve_in=USDC_1_5M,
            reserve_out=WETH_800,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        inp = SolveInput(hops=(v2, v3))
        assert inp.has_v3
        assert not inp.all_v2
        assert inp.v3_indices == (1,)

    def test_frozen(self):
        inp = make_2hop_v2_input()
        with pytest.raises(AttributeError):
            inp.max_input = 100


class TestSolveResult:
    def test_success_result(self):
        result = SolveResult(
            optimal_input=1000,
            profit=500,
            success=True,
            iterations=0,
            method=SolverMethod.MOBIUS,
        )
        assert result.success
        assert result.method == SolverMethod.MOBIUS

    def test_failure_result(self):
        result = SolveResult(
            optimal_input=0,
            profit=0,
            success=False,
            iterations=0,
            method=SolverMethod.MOBIUS,
            error="Not profitable",
        )
        assert not result.success
        assert result.error is not None


# ---------------------------------------------------------------------------
# Test Möbius Coefficients
# ---------------------------------------------------------------------------


class TestMobiusCoefficients:
    def test_profitable_path(self):
        inp = make_2hop_v2_input()
        coeffs = _compute_mobius_coefficients(inp.hops)
        assert coeffs.is_profitable
        assert coeffs.K > coeffs.M

    def test_unprofitable_path(self):
        """Same prices on both sides, high fees: no profit."""
        # Pool 1: USDC → WETH, Pool 2: WETH → USDC
        # With identical prices and 30% fees, K/M = gamma^2 = 0.49 < 1
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=Fraction(30, 100)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(30, 100)),
            )
        )
        coeffs = _compute_mobius_coefficients(inp.hops)
        assert not coeffs.is_profitable

    def test_optimal_input_positive(self):
        inp = make_2hop_v2_input()
        coeffs = _compute_mobius_coefficients(inp.hops)
        x_opt = coeffs.optimal_input()
        assert x_opt > 0

    def test_path_output_round_trip(self):
        """Simulating through hops should match Möbius formula."""
        inp = make_2hop_v2_input()
        coeffs = _compute_mobius_coefficients(inp.hops)
        x = coeffs.optimal_input()
        mobius_output = coeffs.path_output(x)
        sim_output = _simulate_path(x, inp.hops)
        assert mobius_output == pytest.approx(sim_output, rel=1e-10)

    def test_empty_hops(self):
        coeffs = _compute_mobius_coefficients(())
        assert not coeffs.is_profitable
        assert coeffs.optimal_input() == 0.0


# ---------------------------------------------------------------------------
# Test MobiusSolver
# ---------------------------------------------------------------------------


class TestMobiusSolver:
    def test_supports_2hop_v2(self):
        solver = MobiusSolver()
        inp = make_2hop_v2_input()
        assert solver.supports(inp)

    def test_supports_3hop_v2(self):
        solver = MobiusSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_800, reserve_out=500_000 * 10**6, fee=FEE_0_3_PCT),
                Hop(reserve_in=600_000 * 10**6, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            )
        )
        assert solver.supports(inp)

    def test_does_not_support_1hop(self):
        solver = MobiusSolver()
        inp = SolveInput(hops=(Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),))
        assert not solver.supports(inp)

    def test_profitable_solve(self):
        solver = MobiusSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS
        assert result.iterations == 0

    def test_unprofitable_solve(self):
        """Same prices on both sides, high fees: no profit."""
        solver = MobiusSolver()
        # Pool 1: USDC → WETH, Pool 2: WETH → USDC (round trip)
        # With identical prices and 30% fees, round-trip = gamma^2 = 0.49
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=Fraction(30, 100)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(30, 100)),
            )
        )
        result = solver.solve(inp)
        assert not result.success

    def test_max_input_constraint(self):
        solver = MobiusSolver()
        inp = make_2hop_v2_input()
        # Set max_input very low — should constrain
        constrained = SolveInput(hops=inp.hops, max_input=100)
        result = solver.solve(constrained)
        assert result.success
        assert result.optimal_input <= 100

    def test_3hop_solve(self):
        solver = MobiusSolver()
        # 3-hop: USDC → WETH → USDC → WETH (triangular)
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_800, reserve_out=500_000_000_000, fee=FEE_0_3_PCT),
                Hop(reserve_in=500_000_000_000, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.success
        assert result.iterations == 0

    def test_different_fees(self):
        solver = MobiusSolver()
        # Pool 1: buy WETH cheap (lower USDC/WETH ratio), Pool 2: sell WETH expensive
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_5_PCT),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.success


# ---------------------------------------------------------------------------
# Test NewtonSolver
# ---------------------------------------------------------------------------


class TestNewtonSolver:
    def test_supports_2hop_v2(self):
        solver = NewtonSolver()
        inp = make_2hop_v2_input()
        assert solver.supports(inp)

    def test_does_not_support_3hop(self):
        solver = NewtonSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_800, reserve_out=500_000 * 10**6, fee=FEE_0_3_PCT),
                Hop(reserve_in=600_000 * 10**6, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            )
        )
        assert not solver.supports(inp)

    def test_does_not_support_v3(self):
        solver = NewtonSolver()
        v3 = Hop(
            reserve_in=USDC_2M,
            reserve_out=WETH_1000,
            fee=FEE_0_3_PCT,
            liquidity=10**18,
            sqrt_price=2**96,
            tick_lower=-100,
            tick_upper=100,
        )
        v2 = Hop(reserve_in=WETH_800, reserve_out=USDC_1_5M, fee=FEE_0_3_PCT)
        inp = SolveInput(hops=(v3, v2))
        assert not solver.supports(inp)

    def test_profitable_solve(self):
        solver = NewtonSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.method == SolverMethod.NEWTON
        assert result.iterations > 0  # Newton always uses iterations


# ---------------------------------------------------------------------------
# Test BrentSolver
# ---------------------------------------------------------------------------


class TestBrentSolver:
    def test_supports_2hop(self):
        solver = BrentSolver()
        inp = make_2hop_v2_input()
        assert solver.supports(inp)

    def test_profitable_solve(self):
        solver = BrentSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.method == SolverMethod.BRENT


# ---------------------------------------------------------------------------
# Test ArbSolver (dispatcher)
# ---------------------------------------------------------------------------


class TestArbSolver:
    def test_v2_2hop_uses_mobius(self):
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        result = solver.solve(inp)
        assert result.success
        assert result.method == SolverMethod.MOBIUS

    def test_v2_3hop_uses_mobius(self):
        solver = ArbSolver()
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
                Hop(reserve_in=WETH_800, reserve_out=500_000_000_000, fee=FEE_0_3_PCT),
                Hop(reserve_in=500_000_000_000, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            )
        )
        result = solver.solve(inp)
        assert result.success
        assert result.method == SolverMethod.MOBIUS

    def test_unprofitable_returns_failure(self):
        solver = ArbSolver()
        # Round trip: same prices, high fees → no profit
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=Fraction(30, 100)),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=Fraction(30, 100)),
            )
        )
        result = solver.solve(inp)
        assert not result.success

    def test_max_input_respected(self):
        solver = ArbSolver()
        inp = make_2hop_v2_input()
        constrained = SolveInput(hops=inp.hops, max_input=100)
        result = solver.solve(constrained)
        assert result.success
        assert result.optimal_input <= 100


# ---------------------------------------------------------------------------
# Cross-Validation: all solvers agree on the same path
# ---------------------------------------------------------------------------


class TestCrossValidation:
    """All solvers should find the same optimal input and profit for V2-V2."""

    def test_mobius_newton_agree(self):
        inp = make_2hop_v2_input()

        mobius = MobiusSolver().solve(inp)
        newton = NewtonSolver().solve(inp)

        assert mobius.success
        assert newton.success
        # Same profit (flat peak)
        assert mobius.profit == newton.profit
        # Inputs agree within 0.01%
        assert (
            abs(mobius.optimal_input - newton.optimal_input) / max(mobius.optimal_input, 1) < 1e-4
        )

    def test_mobius_brent_agree(self):
        inp = make_2hop_v2_input()

        mobius = MobiusSolver().solve(inp)
        brent = BrentSolver().solve(inp)

        assert mobius.success
        assert brent.success
        # Both find the same profit (flat peak means multiple inputs give same profit)
        assert mobius.profit == brent.profit
        # Inputs agree within 0.01% (flat peak allows small input differences)
        assert abs(mobius.optimal_input - brent.optimal_input) / brent.optimal_input < 1e-4

    def test_newton_brent_agree(self):
        inp = make_2hop_v2_input()

        newton = NewtonSolver().solve(inp)
        brent = BrentSolver().solve(inp)

        assert newton.success
        assert brent.success
        assert newton.profit == brent.profit
        assert abs(newton.optimal_input - brent.optimal_input) / brent.optimal_input < 1e-4

    def test_arb_solver_matches_mobius(self):
        inp = make_2hop_v2_input()

        arb = ArbSolver().solve(inp)
        mobius = MobiusSolver().solve(inp)

        assert arb.success
        assert mobius.success
        assert arb.profit == mobius.profit

    @pytest.mark.parametrize(
        ("fee_buy", "fee_sell"),
        [
            (Fraction(3, 1000), Fraction(3, 1000)),  # 0.3% both
            (Fraction(5, 1000), Fraction(3, 1000)),  # 0.5% / 0.3%
            (Fraction(1, 1000), Fraction(1, 1000)),  # 0.1% both
            (Fraction(10, 10000), Fraction(3, 1000)),  # 0.1% / 0.3%
        ],
    )
    def test_mobius_brent_agree_various_fees(self, fee_buy, fee_sell):
        # Pool 1: USDC → WETH, Pool 2: WETH → USDC
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee_buy),
                Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee_sell),
            )
        )

        mobius = MobiusSolver().solve(inp)
        brent = BrentSolver().solve(inp)

        assert mobius.success
        assert brent.success
        assert mobius.profit == brent.profit
        assert abs(mobius.optimal_input - brent.optimal_input) / brent.optimal_input < 1e-4

    @pytest.mark.parametrize(
        "price_ratio",
        [1.01, 1.02, 1.05, 1.10, 1.20],
    )
    def test_mobius_brent_agree_various_imbalances(self, price_ratio):
        """Test with different price imbalances between the two pools."""
        # Pool 1 (buy WETH cheap): 1.5M USDC / 800 WETH = $1875/WETH
        # Pool 2 (sell WETH expensive): scale WETH reserves DOWN to increase price
        # Lower WETH in pool 2 → higher USDC/WETH price
        weth_pool2 = int(WETH_1000 / price_ratio)
        inp = SolveInput(
            hops=(
                Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
                Hop(reserve_in=weth_pool2, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
            )
        )

        mobius = MobiusSolver().solve(inp)
        brent = BrentSolver().solve(inp)

        assert mobius.success
        assert brent.success
        assert mobius.profit == brent.profit
        assert abs(mobius.optimal_input - brent.optimal_input) / brent.optimal_input < 1e-4
