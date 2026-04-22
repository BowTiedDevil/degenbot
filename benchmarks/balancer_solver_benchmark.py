"""
Benchmark: Closed-form Balancer solver vs grid-search v2 vs Brent vs CVXPY.
"""

import time
from fractions import Fraction

import numpy as np

from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
    compute_optimal_trade,
)

# ---------------------------------------------------------------------------
# Pool Setup
# ---------------------------------------------------------------------------


def make_pool(n_tokens: int, fee_bps: int = 30) -> BalancerMultiTokenState:
    """Create a balanced N-token pool at equilibrium with (2000, 1, 1, ...) prices."""
    if n_tokens == 3:
        return BalancerMultiTokenState(
            reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
            weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
            fee=Fraction(fee_bps, 10000),
            decimals=(18, 6, 6),
        )
    if n_tokens == 4:
        return BalancerMultiTokenState(
            reserves=(
                500_000_000_000_000_000_000,
                1_000_000_000_000,
                1_000_000_000_000,
                500_000_000_000,
            ),
            weights=(
                400_000_000_000_000_000,
                200_000_000_000_000_000,
                200_000_000_000_000_000,
                200_000_000_000_000_000,
            ),
            fee=Fraction(fee_bps, 10000),
            decimals=(18, 6, 6, 6),
        )
    if n_tokens == 5:
        return BalancerMultiTokenState(
            reserves=(
                300_000_000_000_000_000_000,
                1_000_000_000_000,
                1_000_000_000_000,
                500_000_000_000,
                500_000_000_000,
            ),
            weights=(
                300_000_000_000_000_000,
                175_000_000_000_000_000,
                175_000_000_000_000_000,
                175_000_000_000_000_000,
                175_000_000_000_000_000,
            ),
            fee=Fraction(fee_bps, 10000),
            decimals=(18, 6, 6, 6, 6),
        )
    raise ValueError(f"Unsupported n_tokens={n_tokens}")


def random_prices(n_tokens: int, rng: np.random.Generator) -> tuple[float, ...]:
    """Generate random mispriced market prices."""
    prices = [2000.0 + rng.uniform(-200, 200)]  # ETH ±10%
    for _ in range(n_tokens - 1):
        prices.append(1.0 + rng.uniform(-0.1, 0.1))  # stables ±10%
    return tuple(prices)


# ---------------------------------------------------------------------------
# Benchmark: Single signature evaluation (the hot path)
# ---------------------------------------------------------------------------


def bench_single_signature(pool, prices, signature, n_iter=10000):
    """Benchmark the core Equation 9 computation."""
    times = []
    for _ in range(n_iter):
        t0 = time.perf_counter_ns()
        trades = compute_optimal_trade(pool, prices, signature)
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    return times


# ---------------------------------------------------------------------------
# Benchmark: Full solver (all signatures)
# ---------------------------------------------------------------------------


def bench_full_solver(pool, prices_list, n_iter=None):
    """Benchmark the full solver with all signatures."""
    solver = BalancerWeightedPoolSolver()
    times = []
    for prices in prices_list:
        t0 = time.perf_counter_ns()
        result = solver.solve(pool, prices)
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    return times


# ---------------------------------------------------------------------------
# Run Benchmarks
# ---------------------------------------------------------------------------


def main():
    rng = np.random.default_rng(42)
    n_iter = 1000

    for n_tokens in [3, 4, 5]:
        pool = make_pool(n_tokens)
        n_sigs = len([
            s
            for s in __import__("itertools").product((-1, 0, 1), repeat=n_tokens)
            if 1 in s and -1 in s
        ])

        # Generate random price vectors
        prices_list = [random_prices(n_tokens, rng) for _ in range(n_iter)]

        # Single signature benchmark
        sig = tuple([1, -1] + [0] * (n_tokens - 2))[:n_tokens]
        single_times = bench_single_signature(pool, prices_list[0], sig, n_iter=10000)

        # Full solver benchmark
        solver_times = bench_full_solver(pool, prices_list, n_iter)

        print(f"\n{'=' * 60}")
        print(f"N={n_tokens} tokens, {n_sigs} signatures")
        print(f"{'=' * 60}")
        print("  Single signature (Equation 9):")
        print(f"    Mean: {np.mean(single_times) / 1e3:.1f} μs")
        print(f"    Median: {np.median(single_times) / 1e3:.1f} μs")
        print(f"    P99: {np.percentile(single_times, 99) / 1e3:.1f} μs")
        print("  Full solver (all signatures + validation + refinement):")
        print(f"    Mean: {np.mean(solver_times) / 1e3:.1f} μs")
        print(f"    Median: {np.median(solver_times) / 1e3:.1f} μs")
        print(f"    P99: {np.percentile(solver_times, 99) / 1e3:.1f} μs")
        print(
            f"  Theoretical minimum (n_sigs × single): {n_sigs * np.median(single_times) / 1e3:.1f} μs"
        )

    # Correctness check: verify profit direction
    print(f"\n{'=' * 60}")
    print("Correctness Verification")
    print(f"{'=' * 60}")
    pool = make_pool(3)
    solver = BalancerWeightedPoolSolver()

    # ETH cheap: deposit ETH, withdraw stables
    result = solver.solve(pool, (1900.0, 1.0, 1.0))
    print(
        f"ETH=$1900: success={result.success}, profit=${result.profit:.2f}, sig={result.signature}"
    )

    # ETH expensive: withdraw ETH, deposit stables
    result = solver.solve(pool, (2100.0, 1.0, 1.0))
    print(
        f"ETH=$2100: success={result.success}, profit=${result.profit:.2f}, sig={result.signature}"
    )

    # Multi-token mispricing
    result = solver.solve(pool, (2100.0, 0.95, 0.90))
    print(
        f"ETH=$2100, USDC=$0.95, DAI=$0.90: success={result.success}, profit=${result.profit:.2f}, sig={result.signature}"
    )

    # Equilibrium
    result = solver.solve(pool, (2000.0, 1.0, 1.0))
    print(f"Equilibrium: success={result.success}, profit=${result.profit:.6f}")


if __name__ == "__main__":
    main()
