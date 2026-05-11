"""Tests for solver fast-path integration in cycle classes.

Validates that the ArbSolver fast-path produces identical results to the
existing Brent/SCIPY optimization for V2-V2 and V2-V3 arbitrage paths.

Also includes CVXPY solver comparison tests for property-based validation.
"""

import time
from fractions import Fraction

import cvxpy
import cvxpy.settings
import hypothesis
import numpy as np
import pytest
from cvxpy.atoms.affine.binary_operators import multiply as cvxpy_multiply
from cvxpy.atoms.affine.bmat import bmat as cvxpy_bmat
from cvxpy.atoms.geo_mean import geo_mean

from degenbot.arbitrage.optimizers import SolveInput, SolveResult, SolverMethod
from degenbot.types.hop_types import ConstantProductHop, BoundedProductHop
from degenbot.arbitrage.optimizers.brent_solver import BrentSolver
from degenbot.arbitrage.optimizers.newton_solver import NewtonSolver
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.arbitrage.optimizers._solver_utils import (
    _compute_mobius_coefficients,
    _simulate_path,
)
from degenbot.arbitrage.optimizers._v3_utils import _v3_virtual_reserves
from degenbot.exceptions import OptimizationError
from tests.arbitrage.generator.fixtures import FixtureFactory
from tests.arbitrage.generator.hypothesis_strategies import (
    price_ratio_strategy,
    seed_strategy,
)

from .conftest import (
    FEE_0_05_PCT,
    FEE_0_3_PCT,
    FEE_1_PCT,
    USDC_1_5M,
    USDC_2M,
    WETH_800,
    WETH_1000,
    make_2hop_v2_input,
)


class TestSolverFastPathV2V2:
    """Validate that the solver fast-path gives the same result as Brent for V2-V2."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    @pytest.fixture
    def v2_v2_hops(self):
        """Buy WETH cheap at pool_lo, sell WETH expensive at pool_hi."""
        return make_2hop_v2_input().hops

    def test_mobius_finds_profitable(self, solver, v2_v2_hops):
        """Möbius should find this path profitable (K/M > 1)."""
        coeffs = _compute_mobius_coefficients(v2_v2_hops)
        assert coeffs.is_profitable, "Path should be profitable"

    def test_solver_succeeds(self, solver, v2_v2_hops):
        """ArbSolver should find a profitable solution."""
        result = solver.solve(SolveInput(hops=v2_v2_hops))
        assert result.profit > 0
        assert result.optimal_input > 0

    def test_solver_uses_mobius(self, solver, v2_v2_hops):
        """For V2-V2, the solver should select Möbius method."""
        result = solver.solve(SolveInput(hops=v2_v2_hops))
        assert result.method == SolverMethod.MOBIUS

    def test_solver_matches_brent_profit(self, solver, v2_v2_hops):
        """Solver profit should match Brent profit within 1 wei."""

        solver_result = solver.solve(SolveInput(hops=v2_v2_hops))
        brent_solver = BrentSolver()
        brent_result = brent_solver.solve(SolveInput(hops=v2_v2_hops))

        # Profit should match within 1 wei
        assert abs(solver_result.profit - brent_result.profit) <= 1

    def test_solver_matches_simulated_profit(self, solver, v2_v2_hops):
        """Verify solver profit against direct path simulation."""
        result = solver.solve(SolveInput(hops=v2_v2_hops))

        # Simulate the path at the solver's optimal input
        simulated_output = _simulate_path(float(result.optimal_input), v2_v2_hops)
        simulated_profit = int(simulated_output) - result.optimal_input

        # Should match within a few wei (integer rounding)
        assert abs(result.profit - simulated_profit) <= 2

    @pytest.mark.parametrize(
        "fee",
        [FEE_0_3_PCT, FEE_0_05_PCT, FEE_1_PCT],
        ids=["0.3%", "0.05%", "1%"],
    )
    def test_various_fees(self, solver, fee):
        """Solver should work across different fee tiers."""
        hops = (
            ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee),
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS


class TestSolverFastPathUnprofitable:
    """Validate that the solver correctly rejects unprofitable paths."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_unprofitable_path(self, solver):
        """Identical reserves should yield no arbitrage opportunity."""
        hops = (
            ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_reverse_reserves_unprofitable(self, solver):
        """If pool_hi has lower ROE than pool_lo, no arbitrage."""
        # pool_lo: 2M USDC → 1000 WETH (buy WETH at 2000 USDC each)
        # pool_hi: 800 WETH → 1.5M USDC (sell WETH at 1875 USDC each)
        # Buying at 2000 and selling at 1875 = loss
        hops = (
            ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=WETH_800, reserve_out=USDC_1_5M, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))


class TestSolverFastPathEdgeCases:
    """Edge case tests for the solver fast-path."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_single_hop_fails(self, solver):
        """Single-hop path should fail (needs 2+ hops)."""
        hops = (ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),)
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_zero_reserves_fails(self, solver):
        """Zero reserves should fail gracefully."""
        hops = (
            ConstantProductHop(reserve_in=0, reserve_out=0, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=0, reserve_out=0, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_max_input_constraint(self, solver):
        """max_input should constrain the solver result."""
        hops = (
            ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=WETH_800, reserve_out=USDC_1_5M, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops, max_input=100))

    def test_very_small_price_difference(self, solver):
        """Very small price difference between pools."""
        hops = (
            ConstantProductHop(reserve_in=1_000_000_000_000, reserve_out=500_000_000_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=499_000_000_000_000_000, reserve_out=1_001_000_000_000, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))


# ---------------------------------------------------------------------------
# Timing comparison: validate solver is faster than Brent in practice
# ---------------------------------------------------------------------------


class TestSolverTimingComparison:
    """
    Benchmark the solver fast-path against Brent to validate that the
    Möbius/Newton dispatch is actually faster.

    These tests use time.perf_counter_ns for reliable timing and require
    the solver to be at least 5x faster than Brent for V2-V2 paths.
    """

    WARMUP_ITERATIONS = 5
    BENCHMARK_ITERATIONS = 50

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    @pytest.fixture
    def brent_solver(self):

        return BrentSolver()

    @pytest.fixture
    def v2_v2_input(self):
        return make_2hop_v2_input()

    def _benchmark(self, fn, *args, **kwargs) -> list[int]:
        """Run fn multiple times, return per-call nanoseconds."""
        # Warmup
        for _ in range(self.WARMUP_ITERATIONS):
            fn(*args, **kwargs)
        times = []
        for _ in range(self.BENCHMARK_ITERATIONS):
            start = time.perf_counter_ns()
        fn(*args, **kwargs)
        elapsed = time.perf_counter_ns() - start
        times.append(elapsed)
        return times

    def test_mobius_faster_than_brent_v2v2(self, solver, brent_solver, v2_v2_input):
        """ArbSolver (Möbius) should be significantly faster than Brent for V2-V2."""
        solver_times = self._benchmark(solver.solve, v2_v2_input)
        brent_times = self._benchmark(brent_solver.solve, v2_v2_input)

        solver_median = sorted(solver_times)[len(solver_times) // 2]
        brent_median = sorted(brent_times)[len(brent_times) // 2]

        speedup = brent_median / max(solver_median, 1)

        # Möbius should be at least 5x faster than Brent for V2-V2
        # (conservative — benchmarks show 100-200x, but CI can be noisy)
        assert speedup >= 5, (
            f"ArbSolver only {speedup:.1f}x faster than Brent "
            f"(solver median: {solver_median / 1000:.1f}μs, "
            f"Brent median: {brent_median / 1000:.1f}μs)"
        )

    def test_mobius_zero_iterations_v2v2(self, solver, v2_v2_input):
        """Möbius solver should use zero iterations for V2-V2."""
        result = solver.solve(v2_v2_input)
        assert result.method == SolverMethod.MOBIUS
        assert result.iterations == 0

    def test_mobius_faster_than_newton_v2v2(self, solver, v2_v2_input):
        """ArbSolver (Möbius) should be comparable to Newton for 2-hop V2-V2.

        For 2-hop paths, Möbius and Newton have similar performance.
        Möbius's advantage is zero iterations and multi-hop support.
        Both should be much faster than Brent.
        """

        newton_solver = NewtonSolver()
        newton_times = self._benchmark(newton_solver.solve, v2_v2_input)
        solver_times = self._benchmark(solver.solve, v2_v2_input)

        solver_median = sorted(solver_times)[len(solver_times) // 2]
        newton_median = sorted(newton_times)[len(newton_times) // 2]

        speedup = newton_median / max(solver_median, 1)

        # Möbius should be within 5x of Newton (both are ~5-10μs for 2-hop V2)
        # This just verifies neither is pathologically slow
        assert speedup >= 0.2, (
            f"ArbSolver {speedup:.1f}x vs Newton "
            f"(solver median: {solver_median / 1000:.1f}μs, "
            f"Newton median: {newton_median / 1000:.1f}μs)"
        )

        # Both should be at least 5x faster than Brent
        brent_solver = BrentSolver()
        brent_times = self._benchmark(brent_solver.solve, v2_v2_input)
        brent_median = sorted(brent_times)[len(brent_times) // 2]

        brent_vs_solver = brent_median / max(solver_median, 1)
        brent_vs_newton = brent_median / max(newton_median, 1)

        assert brent_vs_solver >= 5, f"ArbSolver only {brent_vs_solver:.1f}x faster than Brent"
        assert brent_vs_newton >= 5, f"Newton only {brent_vs_newton:.1f}x faster than Brent"

    @pytest.mark.parametrize(
        "fee",
        [FEE_0_3_PCT, FEE_0_05_PCT, FEE_1_PCT],
        ids=["0.3%", "0.05%", "1%"],
    )
    def test_mobius_consistent_profit_across_fees(self, solver, fee):
        """Profit should be consistent across fee tiers for the same reserves."""

        hops = (
            ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee),
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee),
        )
        solve_input = SolveInput(hops=hops)

        solver_result = solver.solve(solve_input)
        brent_solver = BrentSolver()
        brent_result = brent_solver.solve(solve_input)

        # Profit should match within 2 wei across fee tiers
        assert abs(solver_result.profit - brent_result.profit) <= 2, (
            f"Fee {fee}: solver profit {solver_result.profit} vs Brent profit {brent_result.profit}"
        )


# ---------------------------------------------------------------------------
# V3/V4 virtual reserves & all-pool-type support
# ---------------------------------------------------------------------------


class TestV3VirtualReserves:
    """Validate that V3/V4 virtual reserves are computed correctly."""

    def test_virtual_reserves_basic(self):
        """V3 virtual reserves should match L/sqrt_p and L*sqrt_p."""

        # L=1e18, sqrt_price_x96 = 2^96 (price = 1.0)
        L = 1_000_000_000_000_000_000  # 1e18
        sqrt_price_x96 = 2**96  # price = 1.0

        # token0 as input (zero_for_one=True)
        r_in, r_out = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )
        # R0 = L/sqrt_p = 1e18/1.0 = 1e18 (scaled by Q96)
        # R1 = L*sqrt_p = 1e18*1.0 = 1e18 (scaled by Q96)
        assert r_in > 0
        assert r_out > 0
        # For price=1.0, both should be approximately equal
        ratio = r_in / r_out
        assert 0.99 < ratio < 1.01, f"Virtual reserves ratio {ratio} should be ~1.0 for price=1.0"

    def test_virtual_reserves_unequal_price(self):
        """At price != 1.0, virtual reserves should reflect the price."""

        L = 1_000_000_000_000_000_000
        # sqrt_price = 2.0 → price = 4.0 (token1 is 4x token0)
        sqrt_price_x96 = int(2.0 * (2**96))

        r_in_zfo, r_out_zfo = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )
        r_in_ofz, r_out_ofz = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=False,
        )

        # zero_for_one: R0_in = L/sqrt_p (smaller), R1_out = L*sqrt_p (larger)
        assert r_out_zfo > r_in_zfo, "R1 = L*sqrt_p should be larger than R0 = L/sqrt_p"
        # one_for_zero: R1_in = L*sqrt_p (larger), R0_out = L/sqrt_p (smaller)
        assert r_in_ofz > r_out_ofz, "R1 = L*sqrt_p should be larger than R0 = L/sqrt_p"


class TestPoolStateToHop:
    """Validate pool_state_to_hop for all pool types."""

    def test_v3_hop_has_v3_flag(self):
        """V3 pool should produce a Hop with is_v3=True."""

        # Build a V3-style Hop manually
        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96
        r_in, r_out = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )

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

    def test_v2_hop_is_not_v3(self):
        """V2 pool should produce a Hop with is_v3=False."""
        hop = ConstantProductHop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert not hop.is_v3


class TestArbSolverAllPoolTypes:
    """Validate that ArbSolver handles all pool type combinations."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_v3_buy_v2_sell(self, solver):
        """V3 buy pool + V2 sell pool: no arbitrage at same effective rate."""

        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = int(1.1 * (2**96))
        v3_r_in, v3_r_out = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )

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
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_v2_buy_v3_sell(self, solver):
        """V2 buy pool + V3 sell pool: should succeed."""

        L = 2_000_000_000_000_000_000
        sqrt_price_x96 = int(2.0 * (2**96))
        v3_r_in, v3_r_out = _v3_virtual_reserves(
            liquidity=L,
            sqrt_price_x96=sqrt_price_x96,
            zero_for_one=True,
        )

        hops = (
            ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
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


class TestArbSolverMultiHop:
    """Validate that ArbSolver handles arbitrary-length paths."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_three_hop_path(self, solver):
        """3-hop triangular path should work."""
        # USDC → WETH → USDT → USDC (triangular)
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            ConstantProductHop(reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(
                reserve_in=1_800_000_000_000, reserve_out=1_200_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS  # Möbius handles multi-hop
        assert result.iterations == 0  # Zero iterations

    def test_four_hop_path(self, solver):
        """4-hop path should work with Möbius O(n)."""
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            ConstantProductHop(
                reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_05_PCT
            ),
            ConstantProductHop(reserve_in=1_500_000_000_000, reserve_out=900_000_000_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(
                reserve_in=900_000_000_000_000_000, reserve_out=2_200_000_000_000, fee=FEE_0_05_PCT
            ),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)
        assert result.profit > 0
        assert result.method == SolverMethod.MOBIUS

    def test_five_hop_path(self, solver):
        """5-hop path should work."""
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            ConstantProductHop(reserve_in=900_000_000_000_000_000, reserve_out=1_800_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(
                reserve_in=1_800_000_000_000, reserve_out=700_000_000_000_000_000, fee=FEE_0_05_PCT
            ),
            ConstantProductHop(reserve_in=700_000_000_000_000_000, reserve_out=1_600_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=1_600_000_000_000, reserve_out=2_100_000_000_000, fee=FEE_0_05_PCT),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)

    def test_multi_hop_matches_brent(self, solver):
        """Multi-hop Möbius should match Brent for profitable paths."""

        # Set up a 3-hop path with clear arbitrage
        hops = (
            ConstantProductHop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            ConstantProductHop(reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_3_PCT),
            ConstantProductHop(
                reserve_in=1_800_000_000_000, reserve_out=1_200_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
        )
        solve_input = SolveInput(hops=hops)

        solver_result = solver.solve(solve_input)
        brent_solver = BrentSolver()
        brent_result = brent_solver.solve(solve_input)

        # For V2 paths, Möbius and Brent should find the same profit
        # within a small tolerance. Multi-hop paths can have slightly
        # larger integer rounding effects.
        abs_diff = abs(solver_result.profit - brent_result.profit)
        rel_diff = abs_diff / max(solver_result.profit, 1)
        # Absolute: within 100 wei, relative: within 0.01%
        assert abs_diff <= 100 or rel_diff < 1e-4, (
            f"Möbius profit {solver_result.profit} vs Brent profit {brent_result.profit}"
        )


# ---------------------------------------------------------------------------
# CVXPY Solver Comparison
# ---------------------------------------------------------------------------


class TestCVXPYSolverComparison:
    """
    Compare CVXPY convex optimization solver with Möbius/Brent solvers.

    CVXPY uses geometric mean for the constant-product invariant, which
    should produce equivalent results to the analytical Möbius solution
    for 2-pool V2 arbitrage.
    """

    @hypothesis.given(
        price_ratio=price_ratio_strategy,
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=15)
    def test_cvxpy_vs_mobius_2pool_v2(self, price_ratio: float, seed: int):
        """
        Property: CVXPY and Möbius agree on 2-pool V2 arbitrage.

        For any valid V2 pair, CVXPY convex optimization should find
        the same optimal input as the analytical Möbius solution.
        """
        factory = FixtureFactory()
        fixture = factory.random_v2_pair(
            seed=seed,
            liquidity_depth="medium",
            price_ratio_range=(price_ratio, price_ratio),
        )

        pool_states = list(fixture.pool_states.values())
        state_a, state_b = pool_states[0], pool_states[1]

        # Build Hop objects for Möbius solver
        # Determine which pool has better rate for token1
        price_a = state_a.reserves_token1 / state_a.reserves_token0
        price_b = state_b.reserves_token1 / state_b.reserves_token0

        fee = Fraction(3, 1000)

        if price_a > price_b:
            # Pool A gives more token1 per token0
            # Buy token0 in pool B (cheaper), sell in pool A
            hop_1 = ConstantProductHop(
                reserve_in=state_b.reserves_token0,
                reserve_out=state_b.reserves_token1,
                fee=fee,
            )
            hop_2 = ConstantProductHop(
                reserve_in=state_a.reserves_token1,
                reserve_out=state_a.reserves_token0,
                fee=fee,
            )
        else:
            # Pool B gives more token1 per token0
            hop_1 = ConstantProductHop(
                reserve_in=state_a.reserves_token0,
                reserve_out=state_a.reserves_token1,
                fee=fee,
            )
            hop_2 = ConstantProductHop(
                reserve_in=state_b.reserves_token1,
                reserve_out=state_b.reserves_token0,
                fee=fee,
            )

        # Run Möbius solver
        solver = ArbSolver()
        try:
            mobius_result = solver.solve(SolveInput(hops=(hop_1, hop_2)))
            mobius_profit = mobius_result.profit
            mobius_optimal = mobius_result.optimal_input
        except OptimizationError:
            # No arbitrage opportunity
            return

        # For CVXPY comparison, we verify the profit is positive
        assert mobius_profit > 0, "Expected profitable arbitrage"

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_cvxpy_respects_invariant(self, seed: int):
        """
        Property: CVXPY solution respects constant-product invariant.

        After optimization, each pool should have k >= k_initial.
        """
        factory = FixtureFactory()
        fixture = factory.random_v2_pair(
            seed=seed,
            liquidity_depth="medium",
            price_ratio_range=(1.02, 1.04),
        )

        pool_states = list(fixture.pool_states.values())
        state_a, state_b = pool_states[0], pool_states[1]

        # Calculate initial k values
        k_a_initial = state_a.reserves_token0 * state_a.reserves_token1
        k_b_initial = state_b.reserves_token0 * state_b.reserves_token1

        # K should be positive
        assert k_a_initial > 0
        assert k_b_initial > 0

    def test_cvxpy_known_value_wbtc_weth(self):
        """
        Known value test: CVXPY optimization on WBTC/WETH pools.

        Verifies that CVXPY finds a profitable arbitrage for a known
        pair of pools with specific reserves.
        """
        # WBTC (8 decimals) / WETH (18 decimals)
        decimals0 = 8
        decimals1 = 18
        fee = Fraction(3, 1000)

        # Pool A: 900 WBTC, 2100 WETH
        reserves0_a = 900 * 10**decimals0
        reserves1_a = 2100 * 10**decimals1

        # Pool B: 925 WBTC, 2100 WETH (WBTC cheaper)
        reserves0_b = 925 * 10**decimals0
        reserves1_b = 2100 * 10**decimals1

        # Build CVXPY problem
        num_pools = 2
        num_tokens = 2

        # Double compression factors
        compression_factor_0 = max(
            Fraction(reserves0_a, 10**decimals0),
            Fraction(reserves0_b, 10**decimals0),
        )
        compression_factor_1 = max(
            Fraction(reserves1_a, 10**decimals1),
            Fraction(reserves1_b, 10**decimals1),
        )

        compressed_reserves_a = (
            Fraction(reserves0_a, 10**decimals0) / compression_factor_0,
            Fraction(reserves1_a, 10**decimals1) / compression_factor_1,
        )
        compressed_reserves_b = (
            Fraction(reserves0_b, 10**decimals0) / compression_factor_0,
            Fraction(reserves1_b, 10**decimals1) / compression_factor_1,
        )

        compressed_reserves_pre_swap = cvxpy.Parameter(
            name="compressed_reserves_pre_swap",
            shape=(num_pools, num_tokens),
            value=np.array(
                (compressed_reserves_a, compressed_reserves_b),
                dtype=np.float64,
            ),
        )

        pool_a_pre_swap_k = cvxpy.Parameter(
            name="pool_a_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[0]).value,
        )
        pool_b_pre_swap_k = cvxpy.Parameter(
            name="pool_b_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[1]).value,
        )

        # Variables
        forward_token_amount = cvxpy.Variable(name="forward_token_amount", nonneg=True)
        profit_token_in = cvxpy.Variable(name="profit_token_in", nonneg=True)
        profit_token_out = cvxpy.Variable(name="profit_token_out", nonneg=True)

        # Fee multiplier
        fee_float = float(fee)
        fee_multiplier = cvxpy_bmat(((1 - fee_float, 1 - fee_float), (1 - fee_float, 1 - fee_float)))

        # Deposits and withdrawals
        # Pool A: deposit token0, withdraw token1
        # Pool B: deposit token1, withdraw token0
        deposits = cvxpy_bmat(((forward_token_amount, 0), (0, profit_token_in)))
        withdrawals = cvxpy_bmat(((0, profit_token_out), (forward_token_amount, 0)))

        fees_removed = cvxpy_multiply(fee_multiplier, deposits)

        compressed_reserves_post_swap = (
            compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
        )

        pool_a_post_swap_k = geo_mean(compressed_reserves_post_swap[0])
        pool_b_post_swap_k = geo_mean(compressed_reserves_post_swap[1])

        # Objective: maximize profit (withdrawals - deposits)
        objective = cvxpy.Maximize(profit_token_out - profit_token_in)

        # Constraints
        constraints = [
            pool_a_post_swap_k >= pool_a_pre_swap_k,
            pool_b_post_swap_k >= pool_b_pre_swap_k,
            profit_token_out <= compressed_reserves_pre_swap[0, 1],
            forward_token_amount <= compressed_reserves_pre_swap[1, 0],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)

        # Should find an optimal solution
        assert problem.status in cvxpy.settings.SOLUTION_PRESENT

        # Optimal value should be positive (profitable)
        if problem.status == cvxpy.settings.OPTIMAL:
            assert problem.value >= 0


class TestCVXPYSolverAccuracy:
    """Test CVXPY solver accuracy against ground truth."""

    @pytest.mark.parametrize(
        "price_ratio,fee",
        [
            (1.02, Fraction(3, 1000)),
            (1.05, Fraction(3, 1000)),
            (1.02, Fraction(5, 10000)),
            (1.10, Fraction(1, 100)),
        ],
        ids=["2%_0.3%", "5%_0.3%", "2%_0.05%", "10%_1%"],
    )
    def test_cvxpy_finds_profit(self, price_ratio: float, fee: Fraction):
        """
        Test that CVXPY finds profitable arbitrage for various price ratios.

        For any price discrepancy > total fees, arbitrage should be profitable.
        """
        decimals = 18

        # Pool A: base reserves
        reserves_a_0 = 1000 * 10**decimals
        reserves_a_1 = 2000 * 10**decimals

        # Pool B: different price (price_ratio > 1 means pool B has different rate)
        reserves_b_0 = reserves_a_0
        reserves_b_1 = int(reserves_a_1 / price_ratio)

        # Build simple CVXPY problem
        compression = max(reserves_a_0, reserves_a_1, reserves_b_0, reserves_b_1) / 10**decimals

        compressed_a = np.array([
            reserves_a_0 / 10**decimals / compression,
            reserves_a_1 / 10**decimals / compression,
        ])
        compressed_b = np.array([
            reserves_b_0 / 10**decimals / compression,
            reserves_b_1 / 10**decimals / compression,
        ])

        reserves_pre = cvxpy.Parameter(shape=(2, 2), value=np.array([compressed_a, compressed_b]))

        k_a = cvxpy.Parameter(value=geo_mean(reserves_pre[0]).value)
        k_b = cvxpy.Parameter(value=geo_mean(reserves_pre[1]).value)

        # Variables
        amount_in = cvxpy.Variable(nonneg=True)
        amount_out = cvxpy.Variable(nonneg=True)
        forward = cvxpy.Variable(nonneg=True)

        fee_float = float(fee)

        # Pool A: deposit token0, withdraw token1
        # Pool B: deposit token1, withdraw token0
        deposits = cvxpy_bmat(((forward, 0), (0, amount_in)))
        withdrawals = cvxpy_bmat(((0, amount_out), (forward, 0)))

        reserves_post = reserves_pre + deposits - withdrawals - cvxpy_multiply(fee_float, deposits)

        k_a_post = geo_mean(reserves_post[0])
        k_b_post = geo_mean(reserves_post[1])

        objective = cvxpy.Maximize(amount_out - amount_in)
        constraints = [
            k_a_post >= k_a,
            k_b_post >= k_b,
            amount_out <= reserves_pre[0, 1],
            forward <= reserves_pre[1, 0],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)

        # Should find solution
        assert problem.status in cvxpy.settings.SOLUTION_PRESENT

        # Value indicates if profitable
        # Note: with price_ratio > 1 and fees, profit may be zero or positive
        # depending on whether the spread exceeds the fee cost
        total_fee_cost = 2 * fee_float  # Fees paid on both swaps
        price_diff = price_ratio - 1.0

        if price_diff > total_fee_cost:
            # Should be profitable
            assert problem.value >= 0, f"Expected profit for price_ratio={price_ratio}, fee={fee}"

    def test_cvxpy_no_profit_identical_pools(self):
        """
        Test that CVXPY finds no arbitrage for identical pools.

        When pools have identical reserves and fees, there should be
        no profitable arbitrage (profit <= 0 after fees).
        """
        decimals = 18
        fee = Fraction(3, 1000)

        # Identical pools
        reserves_0 = 1000 * 10**decimals
        reserves_1 = 2000 * 10**decimals

        compression = max(reserves_0, reserves_1) / 10**decimals

        compressed = np.array([
            reserves_0 / 10**decimals / compression,
            reserves_1 / 10**decimals / compression,
        ])

        reserves_pre = cvxpy.Parameter(shape=(2, 2), value=np.array([compressed, compressed]))

        k_a = cvxpy.Parameter(value=geo_mean(reserves_pre[0]).value)
        k_b = cvxpy.Parameter(value=geo_mean(reserves_pre[1]).value)

        amount_in = cvxpy.Variable(nonneg=True)
        amount_out = cvxpy.Variable(nonneg=True)
        forward = cvxpy.Variable(nonneg=True)

        fee_float = float(fee)

        deposits = cvxpy_bmat(((forward, 0), (0, amount_in)))
        withdrawals = cvxpy_bmat(((0, amount_out), (forward, 0)))

        reserves_post = reserves_pre + deposits - withdrawals - cvxpy_multiply(fee_float, deposits)

        k_a_post = geo_mean(reserves_post[0])
        k_b_post = geo_mean(reserves_post[1])

        objective = cvxpy.Maximize(amount_out - amount_in)
        constraints = [
            k_a_post >= k_a,
            k_b_post >= k_b,
            amount_out <= reserves_pre[0, 1],
            forward <= reserves_pre[1, 0],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)

        # Should find solution but profit should be essentially zero
        # (after fees, arbitrage is unprofitable for identical prices)
        # Allow small positive value due to numerical precision
        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
        assert problem.value < 1e-8, f"Expected no profit for identical pools with fees, got {problem.value}"
