"""
Möbius transformation optimizer tests and benchmarks.

Compares the closed-form Möbius solver against scipy.optimize.minimize_scalar
(Brent) for constant product AMM arbitrage paths of varying length.
"""

import math
import time
from dataclasses import dataclass

import pytest

from degenbot.arbitrage.optimizers.chain_rule import (
    PoolState as ChainPoolState,
)
from degenbot.arbitrage.optimizers.chain_rule import (
    multi_pool_newton_solve,
)
from degenbot.arbitrage.optimizers.mobius import (
    MobiusFloatHop,
    compute_mobius_coefficients,
    mobius_solve,
    simulate_path,
)

from .conftest import brent_solve_hops as brent_solve

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


def chain_state_from_def(pd: PoolDef) -> ChainPoolState:
    return ChainPoolState(
        reserve_in=pd.reserve_in,
        reserve_out=pd.reserve_out,
        fee=pd.fee,
    )


def chain_rule_solve(
    hops: list[MobiusFloatHop],
) -> tuple[float, float, int]:
    """
    Solve using the existing chain rule Newton optimizer.
    """
    pool_states = [chain_state_from_def(MobiusFloatHop(h.reserve_in, h.reserve_out, h.fee)) for h in hops]
    return multi_pool_newton_solve(pool_states)


# ==============================================================================
# Test Fixtures
# ==============================================================================


def profitable_2pool() -> list[PoolDef]:
    """2-pool path with guaranteed profit."""
    return [
        PoolDef(reserve_in=10_000_000.0, reserve_out=5_000.0, fee=0.003),
        PoolDef(reserve_in=4_800.0, reserve_out=11_000_000.0, fee=0.003),
    ]


def profitable_3pool() -> list[PoolDef]:
    """Triangular arbitrage: USDC → WETH → USDT → USDC."""
    return [
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_000.0, fee=0.003),
        PoolDef(reserve_in=500.0, reserve_out=1_000_000.0, fee=0.003),
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_100_000.0, fee=0.003),
    ]


def profitable_4pool() -> list[PoolDef]:
    """4-hop cycle: A → B → C → D → A."""
    return [
        PoolDef(reserve_in=1_000_000.0, reserve_out=2_000.0, fee=0.003),
        PoolDef(reserve_in=1_800.0, reserve_out=5_000.0, fee=0.003),
        PoolDef(reserve_in=4_500.0, reserve_out=800_000.0, fee=0.003),
        PoolDef(reserve_in=750_000.0, reserve_out=1_050_000.0, fee=0.003),
    ]


def profitable_5pool() -> list[PoolDef]:
    """5-hop cycle with varying fees."""
    return [
        PoolDef(reserve_in=2_000_000.0, reserve_out=1_000.0, fee=0.003),
        PoolDef(reserve_in=900.0, reserve_out=3_000.0, fee=0.003),
        PoolDef(reserve_in=2_800.0, reserve_out=10_000.0, fee=0.001),
        PoolDef(reserve_in=9_500.0, reserve_out=500_000.0, fee=0.003),
        PoolDef(reserve_in=450_000.0, reserve_out=2_200_000.0, fee=0.003),
    ]


def unprofitable_3pool() -> list[PoolDef]:
    """3-pool path with no profit (fees consume the opportunity)."""
    return [
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_000.0, fee=0.003),
        PoolDef(reserve_in=1_000.0, reserve_out=1_000_000.0, fee=0.003),
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_000_000.0, fee=0.003),
    ]


def varying_fees_3pool() -> list[PoolDef]:
    """3-pool path with different fees per pool."""
    return [
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_000.0, fee=0.003),
        PoolDef(reserve_in=500.0, reserve_out=1_000_000.0, fee=0.001),
        PoolDef(reserve_in=1_000_000.0, reserve_out=1_100_000.0, fee=0.01),
    ]


def make_n_pool_path(n: int, profit_factor: float = 1.1) -> list[PoolDef]:
    """
    Generate an n-pool cycle with guaranteed profitability.

    Creates a realistic cycle where each pool has slightly mispriced
    reserves, accumulating to an overall profitable cycle. The
    profit_factor amplifies the mispricing per hop.

    The construction ensures each pool has a marginal rate that,
    when composed around the cycle, exceeds 1 (before fees).
    """
    fee = 0.003
    base_reserve = 1_000_000.0

    # Create pools where the cross-rate around the cycle exceeds 1
    # For an n-pool cycle, each pool's rate r_i = s_i/r_i should
    # have a product Π r_i > 1/(1-fee)^n for profitability.
    # We scale each rate by profit_factor^(1/n) so the product = profit_factor.
    per_hop_factor = profit_factor ** (1.0 / n)

    pools: list[PoolDef] = []
    for _i in range(n):
        # Each pool: rate = per_hop_factor * (1-fee)  (slightly favorable)
        # r_in / r_out = 1 / per_hop_factor
        reserve_in = base_reserve
        reserve_out = base_reserve * per_hop_factor
        pools.append(
            PoolDef(
                reserve_in=reserve_in,
                reserve_out=reserve_out,
                fee=fee,
            )
        )

    return pools


# ==============================================================================
# Unit Tests: Möbius Coefficients
# ==============================================================================


class TestMobiusCoefficients:
    """Tests for the core K/M/N coefficient computation."""

    def test_single_hop_matches_v2_formula(self):
        """A 1-hop path should match the direct V2 swap formula."""
        hop = MobiusFloatHop(reserve_in=10_000.0, reserve_out=5_000.0, fee=0.003)
        coeffs = compute_mobius_coefficients([hop])

        # K = gamma * s, M = r, N = gamma
        assert pytest.approx(0.997 * 5_000.0) == coeffs.K
        assert pytest.approx(10_000.0) == coeffs.M
        assert pytest.approx(0.997) == coeffs.N

    def test_two_hop_recovers_known_formula(self):
        """
        For 2 pools (r=a, s=b, fee k) and (r=d, s=c, fee k),
        the coefficients should be K = b·c·k², M = a·d, N = k·(b·k + d).
        """
        k = 1.0 - 0.003  # gamma
        a, b = 1000.0, 2000.0
        d, c = 3000.0, 4000.0

        hops = [
            MobiusFloatHop(reserve_in=a, reserve_out=b, fee=0.003),
            MobiusFloatHop(reserve_in=d, reserve_out=c, fee=0.003),
        ]
        coeffs = compute_mobius_coefficients(hops)

        expected_K = b * c * k**2
        expected_M = a * d
        expected_N = k * (b * k + d)

        assert pytest.approx(expected_K) == coeffs.K
        assert pytest.approx(expected_M) == coeffs.M
        assert pytest.approx(expected_N) == coeffs.N

    def test_profitability_flag_profitable(self):
        """K/M > 1 should set is_profitable = True."""
        hops = [hop_from_def(p) for p in profitable_3pool()]
        coeffs = compute_mobius_coefficients(hops)
        assert coeffs.is_profitable is True
        assert coeffs.K > coeffs.M

    def test_profitability_flag_unprofitable(self):
        """K/M <= 1 should set is_profitable = False."""
        hops = [hop_from_def(p) for p in unprofitable_3pool()]
        coeffs = compute_mobius_coefficients(hops)
        assert coeffs.is_profitable is False

    def test_path_output_matches_simulation(self):
        """Möbius formula output should match hop-by-hop simulation."""
        for fixture_fn in [profitable_2pool, profitable_3pool, profitable_4pool, profitable_5pool]:
            pools = fixture_fn()
            hops = [hop_from_def(p) for p in pools]
            coeffs = compute_mobius_coefficients(hops)

            for x in [1.0, 100.0, 1000.0, 10000.0]:
                mobius_output = coeffs.path_output(x)
                sim_output = simulate_path(x, hops)
                assert mobius_output == pytest.approx(sim_output, rel=1e-10), (
                    f"Möbius vs simulation mismatch at x={x} for {fixture_fn.__name__}"
                )


# ==============================================================================
# Unit Tests: Optimal Input
# ==============================================================================


class TestMobiusSolve:
    """Tests for the closed-form optimal input."""

    @pytest.mark.parametrize(
        "fixture_fn",
        [
            profitable_2pool,
            profitable_3pool,
            profitable_4pool,
            profitable_5pool,
            varying_fees_3pool,
        ],
    )
    def test_optimal_input_positive_for_profitable_paths(self, fixture_fn):
        pools = fixture_fn()
        hops = [hop_from_def(p) for p in pools]
        x_opt, profit, iters = mobius_solve(hops)
        assert x_opt > 0
        assert profit > 0
        assert iters == 0  # Zero iterations — closed form

    def test_unprofitable_path_returns_zero(self):
        pools = unprofitable_3pool()
        hops = [hop_from_def(p) for p in pools]
        x_opt, profit, _iters = mobius_solve(hops)
        assert x_opt == 0.0
        assert profit == 0.0

    def test_max_input_constraint(self):
        pools = profitable_3pool()
        hops = [hop_from_def(p) for p in pools]
        x_unconstrained, _, _ = mobius_solve(hops)
        x_constrained, profit_constrained, _ = mobius_solve(hops, max_input=x_unconstrained * 0.5)
        assert x_constrained <= x_unconstrained * 0.5 + 1e-6
        assert profit_constrained > 0

    def test_gradient_zero_at_optimum(self):
        """The profit gradient should be approximately zero at x_opt."""
        pools = profitable_3pool()
        hops = [hop_from_def(p) for p in pools]
        x_opt, _, _ = mobius_solve(hops)

        # Numerical gradient
        eps = x_opt * 1e-6
        p_plus = simulate_path(x_opt + eps, hops) - (x_opt + eps)
        p_minus = simulate_path(x_opt - eps, hops) - (x_opt - eps)
        gradient = (p_plus - p_minus) / (2 * eps)

        assert abs(gradient) < 1e-4, f"Gradient at optimum should be ~0, got {gradient}"


# ==============================================================================
# Cross-Solver Agreement Tests
# ==============================================================================


class TestMobiusVsBrent:
    """Compare Möbius closed-form against scipy Brent method."""

    @pytest.mark.parametrize(
        "fixture_fn",
        [
            profitable_2pool,
            profitable_3pool,
            profitable_4pool,
            profitable_5pool,
            varying_fees_3pool,
        ],
    )
    def test_optimal_input_agrees_with_brent(self, fixture_fn):
        """Möbius and Brent should find the same optimal input within tolerance."""
        pools = fixture_fn()
        hops = [hop_from_def(p) for p in pools]

        x_mobius, profit_mobius, _ = mobius_solve(hops)
        x_brent, profit_brent, _ = brent_solve(hops)

        # Relative tolerance: both should agree to within 0.01%
        if x_brent > 0:
            rel_diff_x = abs(x_mobius - x_brent) / x_brent
            assert rel_diff_x < 0.0001, (
                f"Input mismatch for {fixture_fn.__name__}: "
                f"Möbius={x_mobius:.2f}, Brent={x_brent:.2f}, rel_diff={rel_diff_x:.6f}"
            )

        if profit_brent > 0:
            rel_diff_p = abs(profit_mobius - profit_brent) / profit_brent
            assert rel_diff_p < 0.0001, (
                f"Profit mismatch for {fixture_fn.__name__}: "
                f"Möbius={profit_mobius:.2f}, Brent={profit_brent:.2f}, rel_diff={rel_diff_p:.6f}"
            )

    @pytest.mark.parametrize("n_pools", [3, 5, 8, 10, 15, 20])
    def test_agreement_scales_with_path_length(self, n_pools):
        """Agreement should hold for longer paths."""
        pools = make_n_pool_path(n_pools)
        hops = [hop_from_def(p) for p in pools]

        _x_mobius, profit_mobius, _ = mobius_solve(hops)
        _x_brent, profit_brent, _ = brent_solve(hops)

        if profit_brent > 0:
            rel_diff = abs(profit_mobius - profit_brent) / profit_brent
            assert rel_diff < 0.001, (
                f"Profit mismatch for {n_pools}-pool path: "
                f"Möbius={profit_mobius:.2f}, Brent={profit_brent:.2f}"
            )


class TestMobiusVsChainRule:
    """Compare Möbius closed-form against chain rule Newton."""

    @pytest.mark.parametrize(
        "fixture_fn",
        [
            profitable_2pool,
            profitable_3pool,
            profitable_4pool,
            profitable_5pool,
            varying_fees_3pool,
        ],
    )
    def test_agrees_with_chain_rule_newton(self, fixture_fn):
        pools = fixture_fn()
        hops = [hop_from_def(p) for p in pools]

        _x_mobius, profit_mobius, _ = mobius_solve(hops)
        _x_chain, profit_chain, _ = chain_rule_solve(hops)

        if profit_chain > 0:
            rel_diff = abs(profit_mobius - profit_chain) / profit_chain
            assert rel_diff < 0.001, (
                f"Profit mismatch for {fixture_fn.__name__}: "
                f"Möbius={profit_mobius:.2f}, ChainRule={profit_chain:.2f}"
            )


# ==============================================================================
# Performance Benchmarks
# ==============================================================================


class TestMobiusBenchmarks:
    """Performance and iteration count benchmarks."""

    def _benchmark_solver(
        self,
        hops: list[MobiusFloatHop],
        solver_fn,
        solver_name: str,
        num_runs: int = 1000,
    ) -> dict:
        """Run a solver many times and collect timing stats."""
        # Warmup
        for _ in range(50):
            solver_fn(hops)

        times = []
        results = []
        for _ in range(num_runs):
            start = time.perf_counter_ns()
            result = solver_fn(hops)
            elapsed = time.perf_counter_ns() - start
            times.append(elapsed)
            results.append(result)

        times_us = [t / 1_000 for t in times]  # convert to μs
        avg_us = sum(times_us) / len(times_us)
        min_us = min(times_us)
        median_us = sorted(times_us)[len(times_us) // 2]

        x_opt, profit, iters = results[len(results) // 2]  # median result

        return {
            "solver": solver_name,
            "avg_us": avg_us,
            "min_us": min_us,
            "median_us": median_us,
            "optimal_input": x_opt,
            "profit": profit,
            "iterations": iters,
            "num_runs": num_runs,
        }

    @pytest.mark.parametrize("n_pools", [2, 3, 4, 5, 10, 20, 50])
    def test_benchmark_mobius_vs_brent(self, n_pools):
        """
        Benchmark Möbius against Brent for varying path lengths.

        This test prints a comparison table. It always passes; the assertions
        are informational.
        """
        pools = make_n_pool_path(n_pools)
        hops = [hop_from_def(p) for p in pools]

        mobius_result = self._benchmark_solver(hops, mobius_solve, "Möbius")
        brent_result = self._benchmark_solver(hops, brent_solve, "Brent")

        speedup = (
            brent_result["median_us"] / mobius_result["median_us"]
            if mobius_result["median_us"] > 0
            else float("inf")
        )

        print(f"\n{'=' * 80}")
        print(f"  BENCHMARK: {n_pools}-pool path")
        print(f"{'=' * 80}")
        print(
            "  Solver       | Median (μs)  | Min (μs)   | Avg (μs)   "
            "| Iterations | Optimal Input  | Profit"
        )
        print(
            "  ------------ | ------------ | ---------- | ---------- "
            "| ---------- | --------------- | ---------------"
        )
        for r in [mobius_result, brent_result]:
            print(
                f"  {r['solver']:<12} | {r['median_us']:<12.2f} | "
                f"{r['min_us']:<10.2f} | {r['avg_us']:<10.2f} | "
                f"{r['iterations']:<10} | {r['optimal_input']:<15.2f} | "
                f"{r['profit']:<15.2f}"
            )
        print(f"\n  Speedup (Möbius vs Brent): {speedup:.1f}x")
        print(
            "  Iteration savings: Brent="
            f"{brent_result['iterations']}, "
            f"Möbius={mobius_result['iterations']}"
        )
        print()

        # Möbius should always use zero iterations
        assert mobius_result["iterations"] == 0

        # Both should find profitable results for profitable paths
        if brent_result["profit"] > 0:
            assert mobius_result["profit"] > 0

    def test_benchmark_mobius_vs_chain_rule(self):
        """Benchmark Möbius against chain rule Newton for 3-6 pool paths."""
        print(f"\n{'=' * 80}")
        print("  BENCHMARK: Möbius vs Chain Rule Newton")
        print(f"{'=' * 80}")
        print("  Pools  | Möbius (μs)  | Chain (μs)  | Möbius Iters | Chain Iters  | Speedup")
        print("  ------ | ------------ | ----------- | ------------ | ------------ | --------")

        for n_pools in [3, 4, 5, 6]:
            pools = make_n_pool_path(n_pools)
            hops = [hop_from_def(p) for p in pools]

            mobius_r = self._benchmark_solver(hops, mobius_solve, "Möbius", num_runs=500)
            chain_r = self._benchmark_solver(hops, chain_rule_solve, "ChainRule", num_runs=500)

            speedup = (
                chain_r["median_us"] / mobius_r["median_us"]
                if mobius_r["median_us"] > 0
                else float("inf")
            )

            print(
                f"  {n_pools:<6} | {mobius_r['median_us']:<12.2f} | "
                f"{chain_r['median_us']:<11.2f} | "
                f"{mobius_r['iterations']:<12} | "
                f"{chain_r['iterations']:<12} | {speedup:<8.1f}x"
            )
        print()

    def test_mobius_profitability_check_is_free(self):
        """
        The profitability check K/M > 1 should require no simulation.
        It's computed as a byproduct of the coefficient recurrence.
        """
        pools = profitable_3pool()
        hops = [hop_from_def(p) for p in pools]
        coeffs = compute_mobius_coefficients(hops)

        # The check is just a comparison, not a simulation
        assert coeffs.is_profitable == (coeffs.K > coeffs.M)

        # For unprofitable path
        pools2 = unprofitable_3pool()
        hops2 = [hop_from_def(p) for p in pools2]
        coeffs2 = compute_mobius_coefficients(hops2)
        assert coeffs2.is_profitable == (coeffs2.K > coeffs2.M)

    def test_mobius_rejects_empty_path(self):
        """Empty hop list should return zero result."""
        x, profit, _iters = mobius_solve([])
        assert x == 0.0
        assert profit == 0.0

    def test_mobius_recovers_2pool_closed_form(self):
        """
        For 2 pools, the Möbius solution should match the known
        2-pool closed-form: x_opt = (k√(abcd) - ad) / (k(bk + d))
        where k = 1 - fee, pool_a = (a, b), pool_b = (d, c).
        """
        fee = 0.003
        k = 1.0 - fee
        a, b = 10_000_000.0, 5_000.0
        d, c = 4_800.0, 11_000_000.0

        hops = [
            MobiusFloatHop(reserve_in=a, reserve_out=b, fee=fee),
            MobiusFloatHop(reserve_in=d, reserve_out=c, fee=fee),
        ]

        x_mobius, _, _ = mobius_solve(hops)
        x_classical = (k * math.sqrt(a * b * c * d) - a * d) / (k * (b * k + d))

        assert x_mobius == pytest.approx(x_classical, rel=1e-10), (
            f"Möbius={x_mobius:.4f}, Classical={x_classical:.4f}"
        )
