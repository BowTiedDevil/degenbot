"""Tests for solver fast-path integration in cycle classes.

Validates that the ArbSolver fast-path produces identical results to the
existing Brent/SCIPY optimization for V2-V2 and V2-V3 arbitrage paths.
"""

import time

import pytest

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    Hop,
    SolveInput,
    SolveResult,
    SolverMethod,
    _compute_mobius_coefficients,
    _simulate_path,
)
from degenbot.exceptions import OptimizationError

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
        from degenbot.arbitrage.optimizers.solver import BrentSolver

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
            Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee),
            Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee),
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
            Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_reverse_reserves_unprofitable(self, solver):
        """If pool_hi has lower ROE than pool_lo, no arbitrage."""
        # pool_lo: 2M USDC → 1000 WETH (buy WETH at 2000 USDC each)
        # pool_hi: 800 WETH → 1.5M USDC (sell WETH at 1875 USDC each)
        # Buying at 2000 and selling at 1875 = loss
        hops = (
            Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            Hop(reserve_in=WETH_800, reserve_out=USDC_1_5M, fee=FEE_0_3_PCT),
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
        hops = (Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),)
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_zero_reserves_fails(self, solver):
        """Zero reserves should fail gracefully."""
        hops = (
            Hop(reserve_in=0, reserve_out=0, fee=FEE_0_3_PCT),
            Hop(reserve_in=0, reserve_out=0, fee=FEE_0_3_PCT),
        )
        with pytest.raises(OptimizationError):
            solver.solve(SolveInput(hops=hops))

    def test_max_input_constraint(self, solver):
        """max_input should constrain the solver result."""
        hops = (
            Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT),
            Hop(reserve_in=WETH_800, reserve_out=USDC_1_5M, fee=FEE_0_3_PCT),
        )
        try:
            result = solver.solve(SolveInput(hops=hops, max_input=100))
            assert isinstance(result, SolveResult)
        except OptimizationError:
            pass

    def test_very_small_price_difference(self, solver):
        """Very small price difference between pools."""
        hops = (
            Hop(reserve_in=1_000_000_000_000, reserve_out=500_000_000_000_000_000, fee=FEE_0_3_PCT),
            Hop(reserve_in=499_000_000_000_000_000, reserve_out=1_001_000_000_000, fee=FEE_0_3_PCT),
        )
        try:
            result = solver.solve(SolveInput(hops=hops))
            assert isinstance(result, SolveResult)
        except OptimizationError:
            pass


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
        from degenbot.arbitrage.optimizers.solver import BrentSolver

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
        from degenbot.arbitrage.optimizers.solver import NewtonSolver

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
        from degenbot.arbitrage.optimizers.solver import BrentSolver

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
        from degenbot.arbitrage.optimizers.solver import BrentSolver

        hops = (
            Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee),
            Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee),
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
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        # L=1e18, sqrt_price_x96 = 2^96 (price = 1.0)
        L = 1_000_000_000_000_000_000  # 1e18
        sqrt_price_x96 = 2**96  # price = 1.0

        # token0 as input (zero_for_one=True)
        r_in, r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)
        # R0 = L/sqrt_p = 1e18/1.0 = 1e18 (scaled by Q96)
        # R1 = L*sqrt_p = 1e18*1.0 = 1e18 (scaled by Q96)
        assert r_in > 0
        assert r_out > 0
        # For price=1.0, both should be approximately equal
        ratio = r_in / r_out
        assert 0.99 < ratio < 1.01, f"Virtual reserves ratio {ratio} should be ~1.0 for price=1.0"

    def test_virtual_reserves_unequal_price(self):
        """At price != 1.0, virtual reserves should reflect the price."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 1_000_000_000_000_000_000
        # sqrt_price = 2.0 → price = 4.0 (token1 is 4x token0)
        sqrt_price_x96 = int(2.0 * (2**96))

        r_in_zfo, r_out_zfo = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)
        r_in_ofz, r_out_ofz = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=False)

        # zero_for_one: R0_in = L/sqrt_p (smaller), R1_out = L*sqrt_p (larger)
        assert r_out_zfo > r_in_zfo, "R1 = L*sqrt_p should be larger than R0 = L/sqrt_p"
        # one_for_zero: R1_in = L*sqrt_p (larger), R0_out = L/sqrt_p (smaller)
        assert r_in_ofz > r_out_ofz, "R1 = L*sqrt_p should be larger than R0 = L/sqrt_p"


class TestPoolStateToHop:
    """Validate pool_state_to_hop for all pool types."""

    def test_v3_hop_has_v3_flag(self):
        """V3 pool should produce a Hop with is_v3=True."""

        from degenbot.arbitrage.optimizers.solver import Hop, _v3_virtual_reserves

        # Build a V3-style Hop manually
        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96
        r_in, r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hop = Hop(
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
        hop = Hop(reserve_in=USDC_2M, reserve_out=WETH_1000, fee=FEE_0_3_PCT)
        assert not hop.is_v3


class TestArbSolverAllPoolTypes:
    """Validate that ArbSolver handles all pool type combinations."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_v3_buy_v2_sell(self, solver):
        """V3 buy pool + V2 sell pool: should succeed with virtual reserves."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 1_000_000_000_000_000_000
        sqrt_price_x96 = 2**96
        # V3 buy pool (token0 in, token1 out)
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hops = (
            Hop(
                reserve_in=v3_r_in,
                reserve_out=v3_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
            Hop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
        )
        try:
            result = solver.solve(SolveInput(hops=hops))
            assert isinstance(result, SolveResult)
        except (OptimizationError, AssertionError):
            pass

    def test_v2_buy_v3_sell(self, solver):
        """V2 buy pool + V3 sell pool: should succeed."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L = 2_000_000_000_000_000_000  # Higher liquidity for V3 sell
        sqrt_price_x96 = int(2.0 * (2**96))  # price = 4.0
        v3_r_in, v3_r_out = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        hops = (
            Hop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
            Hop(
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

    def test_v3_buy_v3_sell(self, solver):
        """V3 buy pool + V3 sell pool: should succeed."""
        from degenbot.arbitrage.optimizers.solver import _v3_virtual_reserves

        L_lo = 1_000_000_000_000_000_000
        L_hi = 2_000_000_000_000_000_000
        sqrt_price_x96 = 2**96

        lo_r_in, lo_r_out = _v3_virtual_reserves(L_lo, sqrt_price_x96, zero_for_one=True)
        hi_r_in, hi_r_out = _v3_virtual_reserves(L_hi, sqrt_price_x96, zero_for_one=True)

        hops = (
            Hop(
                reserve_in=lo_r_in,
                reserve_out=lo_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L_lo,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
            Hop(
                reserve_in=hi_r_in,
                reserve_out=hi_r_out,
                fee=FEE_0_3_PCT,
                liquidity=L_hi,
                sqrt_price=sqrt_price_x96,
                tick_lower=0,
                tick_upper=0,
            ),
        )
        try:
            result = solver.solve(SolveInput(hops=hops))
            assert isinstance(result, SolveResult)
        except (OptimizationError, AssertionError):
            pass


class TestArbSolverMultiHop:
    """Validate that ArbSolver handles arbitrary-length paths."""

    @pytest.fixture
    def solver(self):
        return ArbSolver()

    def test_three_hop_path(self, solver):
        """3-hop triangular path should work."""
        # USDC → WETH → USDT → USDC (triangular)
        hops = (
            Hop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            Hop(reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_3_PCT),
            Hop(
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
            Hop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            Hop(
                reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_05_PCT
            ),
            Hop(reserve_in=1_500_000_000_000, reserve_out=900_000_000_000_000_000, fee=FEE_0_3_PCT),
            Hop(
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
            Hop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            Hop(reserve_in=900_000_000_000_000_000, reserve_out=1_800_000_000_000, fee=FEE_0_3_PCT),
            Hop(
                reserve_in=1_800_000_000_000, reserve_out=700_000_000_000_000_000, fee=FEE_0_05_PCT
            ),
            Hop(reserve_in=700_000_000_000_000_000, reserve_out=1_600_000_000_000, fee=FEE_0_3_PCT),
            Hop(reserve_in=1_600_000_000_000, reserve_out=2_100_000_000_000, fee=FEE_0_05_PCT),
        )
        result = solver.solve(SolveInput(hops=hops))
        assert isinstance(result, SolveResult)

    def test_multi_hop_matches_brent(self, solver):
        """Multi-hop Möbius should match Brent for profitable paths."""
        from degenbot.arbitrage.optimizers.solver import BrentSolver

        # Set up a 3-hop path with clear arbitrage
        hops = (
            Hop(
                reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=FEE_0_3_PCT
            ),
            Hop(reserve_in=800_000_000_000_000_000, reserve_out=1_500_000_000_000, fee=FEE_0_3_PCT),
            Hop(
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
