"""Benchmark: registered path solve vs. all other approaches.

Compares:
1. Individual cache.solve() per path
2. cache.solve_batch() 
3. cache.solve_registered() (pre-resolved paths)
4. ArbSolver.solve_cached_batch()
5. Individual solve_raw()

Key question: does pre-resolving paths (register_path + solve_registered)
eliminate enough per-solve overhead to matter?
"""

import time

import degenbot.degenbot_rs as rs_mobius
from degenbot.arbitrage.optimizers.solver import ArbSolver
from fractions import Fraction

USDC_DECIMALS = 6
WETH_DECIMALS = 18
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
USDC_2M = 2_000_000 * 10**USDC_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS
GAMMA_03 = 997
FEE_DENOM = 1000

NUM_WARMUP = 100
NUM_ITERATIONS = 5000


def bench_registered_vs_all():
    """Compare all solve approaches side by side."""
    num_paths = 50

    # Setup cache with pools
    cache = rs_mobius.RustPoolCache()
    pool_paths = []
    registered_path_ids = []
    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        cache.insert(
            i * 2,
            int(USDC_1_5M * factor),
            int(WETH_800 * factor),
            GAMMA_03,
            FEE_DENOM,
        )
        cache.insert(
            i * 2 + 1,
            int(WETH_1000 * factor),
            int(USDC_2M * factor),
            GAMMA_03,
            FEE_DENOM,
        )
        pool_paths.append([i * 2, i * 2 + 1])
        pid = cache.register_path([i * 2, i * 2 + 1])
        registered_path_ids.append(pid)

    # Warmup all approaches
    for _ in range(NUM_WARMUP):
        for path in pool_paths:
            cache.solve(path)
        cache.solve_batch(pool_paths)
        cache.solve_registered(registered_path_ids)

    iterations = NUM_ITERATIONS

    # 1. Individual cache.solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        for path in pool_paths:
            cache.solve(path)
    indiv_ns = time.perf_counter_ns() - start

    # 2. cache.solve_batch
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve_batch(pool_paths)
    batch_ns = time.perf_counter_ns() - start

    # 3. cache.solve_registered
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve_registered(registered_path_ids)
    registered_ns = time.perf_counter_ns() - start

    print("=" * 72)
    print(f"Comparison: {num_paths} paths, {iterations} iterations")
    print("=" * 72)

    indiv_per_path = indiv_ns / iterations / num_paths
    batch_per_path = batch_ns / iterations / num_paths
    reg_per_path = registered_ns / iterations / num_paths

    print(f"  Individual cache.solve():  {indiv_ns / iterations:>10,.0f} ns total  ({indiv_per_path:>6,.0f} ns/path)")
    print(f"  cache.solve_batch():       {batch_ns / iterations:>10,.0f} ns total  ({batch_per_path:>6,.0f} ns/path)  {indiv_per_path / batch_per_path:.2f}x")
    print(f"  cache.solve_registered():  {registered_ns / iterations:>10,.0f} ns total  ({reg_per_path:>6,.0f} ns/path)  {indiv_per_path / reg_per_path:.2f}x")

    # 4. With path update (simulating block update + solve)
    print()
    print("Block update cycle: update_all_paths() + solve_registered()")
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.update_all_paths()
        cache.solve_registered(registered_path_ids)
    update_solve_ns = time.perf_counter_ns() - start

    # For comparison: update pool states + individual solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        for path in pool_paths:
            cache.solve(path)
    update_indiv_ns = time.perf_counter_ns() - start

    update_solve_per_path = update_solve_ns / iterations / num_paths
    print(f"  update_all + solve_registered: {update_solve_ns / iterations:>10,.0f} ns  ({update_solve_per_path:>6,.0f} ns/path)")
    print(f"  Individual (no update):        {update_indiv_ns / iterations:>10,.0f} ns  ({update_indiv_ns / iterations / num_paths:>6,.0f} ns/path)")

    return reg_per_path, batch_per_path, indiv_per_path


def bench_registered_scaling():
    """How does solve_registered scale with batch size?"""
    print()
    print("=" * 72)
    print("solve_registered scaling (batch size → ns/path)")
    print("=" * 72)

    for batch_size in [1, 2, 5, 10, 20, 50, 100, 200]:
        cache = rs_mobius.RustPoolCache()
        pool_paths = []
        registered_path_ids = []
        for i in range(batch_size):
            factor = 1.0 + (i % 10) * 0.05
            cache.insert(
                i * 2,
                int(USDC_1_5M * factor),
                int(WETH_800 * factor),
                GAMMA_03,
                FEE_DENOM,
            )
            cache.insert(
                i * 2 + 1,
                int(WETH_1000 * factor),
                int(USDC_2M * factor),
                GAMMA_03,
                FEE_DENOM,
            )
            pool_paths.append([i * 2, i * 2 + 1])
            pid = cache.register_path([i * 2, i * 2 + 1])
            registered_path_ids.append(pid)

        # Warmup
        for _ in range(NUM_WARMUP):
            cache.solve_registered(registered_path_ids)
            cache.solve_batch(pool_paths)

        iterations = max(100, NUM_ITERATIONS // max(1, batch_size // 5))

        # Benchmark registered
        start = time.perf_counter_ns()
        for _ in range(iterations):
            cache.solve_registered(registered_path_ids)
        reg_ns = (time.perf_counter_ns() - start) / iterations

        # Benchmark batch for comparison
        start = time.perf_counter_ns()
        for _ in range(iterations):
            cache.solve_batch(pool_paths)
        batch_ns = (time.perf_counter_ns() - start) / iterations

        # Benchmark individual
        start = time.perf_counter_ns()
        for _ in range(iterations):
            for path in pool_paths:
                cache.solve(path)
        indiv_ns = (time.perf_counter_ns() - start) / iterations

        reg_per_path = reg_ns / batch_size
        batch_per_path = batch_ns / batch_size
        indiv_per_path = indiv_ns / batch_size

        print(f"  batch_size={batch_size:>3}:  reg={reg_per_path:>6,.0f} ns/path  "
              f"batch={batch_per_path:>6,.0f} ns/path  "
              f"indiv={indiv_per_path:>6,.0f} ns/path  "
              f"speedup={indiv_per_path / reg_per_path:.2f}x")


def bench_arb_solver_registered():
    """ArbSolver with registered paths vs other approaches."""
    print()
    print("=" * 72)
    print("ArbSolver: registered paths vs batch vs individual")
    print("=" * 72)

    fee = Fraction(3, 1000)

    solver = ArbSolver()
    cache = solver.get_pool_cache()
    num_paths = 50
    pool_paths = []
    registered_path_ids = []
    for i in range(num_paths):
        factor = 1.0 + (i % 10) * 0.05
        pid0 = solver.register_pool(int(USDC_1_5M * factor), int(WETH_800 * factor), fee)
        pid1 = solver.register_pool(int(WETH_1000 * factor), int(USDC_2M * factor), fee)
        pool_paths.append([pid0, pid1])
        rpath_id = cache.register_path([pid0, pid1])
        registered_path_ids.append(rpath_id)

    # Warmup
    for _ in range(NUM_WARMUP):
        for path in pool_paths:
            try:
                solver.solve_cached(path)
            except Exception:
                pass
        solver.solve_cached_batch(pool_paths)
        cache.solve_registered(registered_path_ids)

    iterations = NUM_ITERATIONS

    # Individual solve_cached
    start = time.perf_counter_ns()
    for _ in range(iterations):
        for path in pool_paths:
            try:
                solver.solve_cached(path)
            except Exception:
                pass
    indiv_ns = time.perf_counter_ns() - start

    # Batch
    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve_cached_batch(pool_paths)
    batch_ns = time.perf_counter_ns() - start

    # Registered (raw cache)
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve_registered(registered_path_ids)
    reg_ns = time.perf_counter_ns() - start

    indiv_per_path = indiv_ns / iterations / num_paths
    batch_per_path = batch_ns / iterations / num_paths
    reg_per_path = reg_ns / iterations / num_paths

    print(f"  Individual solve_cached:    {indiv_ns / iterations:>10,.0f} ns  ({indiv_per_path:>6,.0f} ns/path)")
    print(f"  solve_cached_batch:         {batch_ns / iterations:>10,.0f} ns  ({batch_per_path:>6,.0f} ns/path)  {indiv_per_path / batch_per_path:.2f}x")
    print(f"  cache.solve_registered:     {reg_ns / iterations:>10,.0f} ns  ({reg_per_path:>6,.0f} ns/path)  {indiv_per_path / reg_per_path:.2f}x")


if __name__ == "__main__":
    bench_registered_vs_all()
    bench_registered_scaling()
    bench_arb_solver_registered()
