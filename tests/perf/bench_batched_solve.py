"""Benchmark: batched Rust solve vs. per-path solve.

Measures the performance improvement from solving multiple arbitrage
paths in a single Python → Rust → Python round-trip (batched) vs.
the current per-path approach.

Key hypothesis: batching amortizes ~1,160ns PyO3 bridge overhead
across N paths, achieving near-Rust-native throughput for the solve
computation itself.
"""

import time

import degenbot.degenbot_rs as rs_mobius
from degenbot.arbitrage.optimizers.solver import ArbSolver

# ==============================================================================
# Constants
# ==============================================================================

USDC_DECIMALS = 6
WETH_DECIMALS = 18
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
USDC_2M = 2_000_000 * 10**USDC_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS
FEE_DENOM = 1000
GAMMA_03 = 997

NUM_WARMUP = 50
NUM_ITERATIONS = 1000


def bench_solve_raw_batch_vs_per_path():
    """Compare solve_raw_batch vs. individual solve_raw calls."""
    solver = rs_mobius.RustArbSolver()

    # Build N paths with varying reserves
    num_paths = 50
    paths = []
    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        flat = [
            int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM,
            int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM,
        ]
        paths.append(flat)

    # Warmup
    for _ in range(NUM_WARMUP):
        for flat in paths:
            solver.solve_raw(flat)
        solver.solve_raw_batch(paths)

    # Benchmark per-path
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        for flat in paths:
            solver.solve_raw(flat)
    per_path_ns = time.perf_counter_ns() - start

    # Benchmark batched
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        solver.solve_raw_batch(paths)
    batch_ns = time.perf_counter_ns() - start

    per_path_per_iter = per_path_ns / NUM_ITERATIONS
    batch_per_iter = batch_ns / NUM_ITERATIONS
    per_path_per_path = per_path_per_iter / num_paths
    batch_per_path = batch_per_iter / num_paths

    print("=" * 70)
    print("solve_raw_batch vs. per-path solve_raw")
    print("=" * 70)
    print(f"Paths per batch: {num_paths}")
    print(f"Iterations:      {NUM_ITERATIONS}")
    print()
    print(f"Per-path total:  {per_path_per_iter:>12,.0f} ns  ({per_path_per_path:>8,.0f} ns/path)")
    print(f"Batch total:     {batch_per_iter:>12,.0f} ns  ({batch_per_path:>8,.0f} ns/path)")
    print()
    speedup = per_path_per_path / batch_per_path if batch_per_path > 0 else float("inf")
    print(f"Speedup:         {speedup:.2f}x")
    print(f"Time saved:      {per_path_per_iter - batch_per_iter:,.0f} ns per batch ({(1 - batch_per_iter / per_path_per_iter) * 100:.1f}%)")


def bench_cache_batch_vs_per_path():
    """Compare cache.solve_batch vs. individual cache.solve calls."""
    cache = rs_mobius.RustPoolCache()

    # Register pools
    num_paths = 50
    paths = []
    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        cache.insert(i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM)
        cache.insert(i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM)
        paths.append([i * 2, i * 2 + 1])

    # Warmup
    for _ in range(NUM_WARMUP):
        for path in paths:
            cache.solve(path)
        cache.solve_batch(paths)

    # Benchmark per-path
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        for path in paths:
            cache.solve(path)
    per_path_ns = time.perf_counter_ns() - start

    # Benchmark batched
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        cache.solve_batch(paths)
    batch_ns = time.perf_counter_ns() - start

    per_path_per_iter = per_path_ns / NUM_ITERATIONS
    batch_per_iter = batch_ns / NUM_ITERATIONS
    per_path_per_path = per_path_per_iter / num_paths
    batch_per_path = batch_per_iter / num_paths

    print()
    print("=" * 70)
    print("cache.solve_batch vs. per-path cache.solve")
    print("=" * 70)
    print(f"Paths per batch: {num_paths}")
    print(f"Iterations:      {NUM_ITERATIONS}")
    print()
    print(f"Per-path total:  {per_path_per_iter:>12,.0f} ns  ({per_path_per_path:>8,.0f} ns/path)")
    print(f"Batch total:     {batch_per_iter:>12,.0f} ns  ({batch_per_path:>8,.0f} ns/path)")
    print()
    speedup = per_path_per_path / batch_per_path if batch_per_path > 0 else float("inf")
    print(f"Speedup:         {speedup:.2f}x")
    print(f"Time saved:      {per_path_per_iter - batch_per_iter:,.0f} ns per batch ({(1 - batch_per_iter / per_path_per_iter) * 100:.1f}%)")


def bench_cache_batch_scaling():
    """How does batch performance scale with batch size?"""
    print()
    print("=" * 70)
    print("cache.solve_batch scaling (batch size → ns/path)")
    print("=" * 70)

    for batch_size in [1, 2, 5, 10, 20, 50, 100]:
        cache = rs_mobius.RustPoolCache()
        paths = []
        for i in range(batch_size):
            factor = 1.0 + (i % 10) * 0.05
            cache.insert(i * 2, int(USDC_1_5M * factor), int(WETH_800 * factor), GAMMA_03, FEE_DENOM)
            cache.insert(i * 2 + 1, int(WETH_1000 * factor), int(USDC_2M * factor), GAMMA_03, FEE_DENOM)
            paths.append([i * 2, i * 2 + 1])

        # Warmup
        for _ in range(NUM_WARMUP):
            cache.solve_batch(paths)

        iterations = max(100, NUM_ITERATIONS // max(1, batch_size))

        # Benchmark batched
        start = time.perf_counter_ns()
        for _ in range(iterations):
            cache.solve_batch(paths)
        batch_ns = time.perf_counter_ns() - start

        # Benchmark per-path for comparison
        start = time.perf_counter_ns()
        for _ in range(iterations):
            for path in paths:
                cache.solve(path)
        per_path_ns = time.perf_counter_ns() - start

        batch_per_path = (batch_ns / iterations) / batch_size
        per_path_each = (per_path_ns / iterations) / batch_size
        speedup = per_path_each / batch_per_path if batch_per_path > 0 else float("inf")

        print(f"  batch_size={batch_size:>3}:  batch={batch_per_path:>8,.0f} ns/path  "
              f"per-path={per_path_each:>8,.0f} ns/path  speedup={speedup:.2f}x")


def bench_arb_solver_batch():
    """Compare ArbSolver.solve_cached_batch vs. per-path solve_cached."""
    print()
    print("=" * 70)
    print("ArbSolver.solve_cached_batch vs. per-path solve_cached")
    print("=" * 70)

    from fractions import Fraction
    fee = Fraction(3, 1000)

    solver = ArbSolver()
    num_paths = 50
    paths = []
    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), fee)
        pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), fee)
        paths.append([pid0, pid1])

    # Warmup
    for _ in range(NUM_WARMUP):
        for path in paths:
            try:
                solver.solve_cached(path)
            except Exception:
                pass
        solver.solve_cached_batch(paths)

    # Benchmark per-path
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        for path in paths:
            try:
                solver.solve_cached(path)
            except Exception:
                pass
    per_path_ns = time.perf_counter_ns() - start

    # Benchmark batched
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        solver.solve_cached_batch(paths)
    batch_ns = time.perf_counter_ns() - start

    per_path_per_iter = per_path_ns / NUM_ITERATIONS
    batch_per_iter = batch_ns / NUM_ITERATIONS
    per_path_per_path = per_path_per_iter / num_paths
    batch_per_path = batch_per_iter / num_paths

    print(f"Paths per batch: {num_paths}")
    print(f"Iterations:      {NUM_ITERATIONS}")
    print()
    print(f"Per-path total:  {per_path_per_iter:>12,.0f} ns  ({per_path_per_path:>8,.0f} ns/path)")
    print(f"Batch total:     {batch_per_iter:>12,.0f} ns  ({batch_per_path:>8,.0f} ns/path)")
    print()
    speedup = per_path_per_path / batch_per_path if batch_per_path > 0 else float("inf")
    print(f"Speedup:         {speedup:.2f}x")


if __name__ == "__main__":
    bench_solve_raw_batch_vs_per_path()
    bench_cache_batch_vs_per_path()
    bench_cache_batch_scaling()
    bench_arb_solver_batch()
