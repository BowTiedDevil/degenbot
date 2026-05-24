"""Micro-benchmark: where does the time go inside cache.solve_batch?

Instruments each phase of the batched solve pipeline:
1. Python → Rust path list extraction
2. Cache lock + lookup (GIL-held)
3. GIL-released solve computation
4. Rust → Python result construction

Compares against the per-path approach to quantify each phase's savings.
"""

import time

import degenbot.degenbot_rs as rs_mobius

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


def bench_per_path_overhead():
    """Measure overhead in individual cache.solve() calls."""
    cache = rs_mobius.RustPoolCache()
    num_pools = 100
    for i in range(num_pools):
        factor = 1.0 + (i % 10) * 0.05
        cache.insert(
            i,
            int(USDC_1_5M * factor),
            int(WETH_800 * factor),
            GAMMA_03,
            FEE_DENOM,
        )

    # Warmup
    for _ in range(NUM_WARMUP):
        for i in range(0, num_pools - 1, 2):
            cache.solve([i, i + 1])

    # Measure
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        for i in range(0, num_pools - 1, 2):
            cache.solve([i, i + 1])
    total_ns = time.perf_counter_ns() - start
    num_paths = num_pools // 2
    total_calls = NUM_ITERATIONS * num_paths
    per_call_ns = total_ns / total_calls

    print(f"Individual cache.solve(): {per_call_ns:,.0f} ns/call")
    return per_call_ns


def bench_batch_overhead():
    """Measure overhead in cache.solve_batch() calls."""
    cache = rs_mobius.RustPoolCache()
    num_pools = 100
    for i in range(num_pools):
        factor = 1.0 + (i % 10) * 0.05
        cache.insert(
            i,
            int(USDC_1_5M * factor),
            int(WETH_800 * factor),
            GAMMA_03,
            FEE_DENOM,
        )

    paths = [[i, i + 1] for i in range(0, num_pools - 1, 2)]
    num_paths = len(paths)

    # Warmup
    for _ in range(NUM_WARMUP):
        cache.solve_batch(paths)

    # Measure
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        cache.solve_batch(paths)
    total_ns = time.perf_counter_ns() - start
    per_batch_ns = total_ns / NUM_ITERATIONS
    per_path_ns = per_batch_ns / num_paths

    print(f"Batch cache.solve_batch(): {per_batch_ns:,.0f} ns/batch ({per_path_ns:,.0f} ns/path)")
    return per_batch_ns, num_paths


def bench_solve_raw_overhead():
    """Measure overhead in individual solve_raw() calls (the baseline from original investigation)."""
    solver = rs_mobius.RustArbSolver()
    flat = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM,
            WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM]

    # Warmup
    for _ in range(NUM_WARMUP):
        solver.solve_raw(flat)

    # Measure
    start = time.perf_counter_ns()
    for _ in range(NUM_ITERATIONS):
        solver.solve_raw(flat)
    total_ns = time.perf_counter_ns() - start
    per_call_ns = total_ns / NUM_ITERATIONS

    print(f"Individual solve_raw(): {per_call_ns:,.0f} ns/call")
    return per_call_ns


def bench_cache_solve_vs_solve_raw():
    """Compare the two fastest paths: cache.solve vs solve_raw."""
    # Setup cache
    cache = rs_mobius.RustPoolCache()
    cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM)
    cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM)

    # Setup solver
    solver = rs_mobius.RustArbSolver()
    flat = [USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM,
            WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM]

    # Warmup
    for _ in range(NUM_WARMUP):
        cache.solve([0, 1])
        solver.solve_raw(flat)

    iterations = 20000

    # Measure cache.solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve([0, 1])
    cache_ns = time.perf_counter_ns() - start

    # Measure solve_raw
    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve_raw(flat)
    raw_ns = time.perf_counter_ns() - start

    print(f"\ncache.solve([0,1]):     {cache_ns / iterations:,.0f} ns/call")
    print(f"solve_raw(flat):        {raw_ns / iterations:,.0f} ns/call")
    print(f"Ratio (cache/raw):      {cache_ns / raw_ns:.2f}x")


def bench_result_construction():
    """Isolate the cost of Rust → Python result construction."""
    cache = rs_mobius.RustPoolCache()
    cache.insert(0, USDC_1_5M, WETH_800, GAMMA_03, FEE_DENOM)
    cache.insert(1, WETH_1000, USDC_2M, GAMMA_03, FEE_DENOM)

    # Get a result to see its structure
    result = cache.solve([0, 1])
    print(f"\nResult fields: optimal_input={result.optimal_input}, "
          f"profit={result.profit}, iterations={result.iterations}, "
          f"success={result.success}, method={result.method}, "
          f"supported={result.supported}")
    print(f"  optimal_input_int type: {type(result.optimal_input_int)}")
    print(f"  profit_int type: {type(result.profit_int)}")

    # Measure just the int() conversion (Python side of result construction)
    iterations = 100000

    start = time.perf_counter_ns()
    for _ in range(iterations):
        int(result.optimal_input_int)
    input_conv_ns = (time.perf_counter_ns() - start) / iterations

    start = time.perf_counter_ns()
    for _ in range(iterations):
        int(result.profit_int)
    profit_conv_ns = (time.perf_counter_ns() - start) / iterations

    print(f"  int(optimal_input_int): {input_conv_ns:,.0f} ns")
    print(f"  int(profit_int):        {profit_conv_ns:,.0f} ns")
    print(f"  Total int() conversion: {input_conv_ns + profit_conv_ns:,.0f} ns")


def bench_python_list_overhead():
    """Measure the Python list-of-lists construction and extraction overhead."""
    num_paths = 50
    paths = [[i * 2, i * 2 + 1] for i in range(num_paths)]

    iterations = 50000

    # Measure: Python list → Rust Vec<Vec<u64>> extraction
    # We'll measure the cache.solve_batch overhead vs pure Rust computation
    # by comparing with a direct Rust call that skips extraction

    cache = rs_mobius.RustPoolCache()
    for i in range(num_paths * 2):
        factor = 1.0 + (i % 10) * 0.05
        cache.insert(
            i,
            int(USDC_1_5M * factor),
            int(WETH_800 * factor),
            GAMMA_03,
            FEE_DENOM,
        )

    # Warmup
    for _ in range(100):
        cache.solve_batch(paths)

    # Measure batch with 50 paths
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve_batch(paths)
    batch_50_ns = (time.perf_counter_ns() - start) / iterations

    # Measure batch with 1 path
    single_path = [paths[0]]
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve_batch(single_path)
    batch_1_ns = (time.perf_counter_ns() - start) / iterations

    # Measure single cache.solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve(paths[0])
    solve_1_ns = (time.perf_counter_ns() - start) / iterations

    print(f"\nPython list overhead analysis:")
    print(f"  cache.solve(1 path):             {solve_1_ns:,.0f} ns")
    print(f"  cache.solve_batch([1 path]):     {batch_1_ns:,.0f} ns")
    print(f"  cache.solve_batch([50 paths]):   {batch_50_ns:,.0f} ns total ({batch_50_ns / 50:,.0f} ns/path)")
    print(f"  Overhead of batch vs per-path:    {batch_1_ns - solve_1_ns:,.0f} ns for 1-path batch")
    print(f"  Marginal cost per additional path: {(batch_50_ns - batch_1_ns) / 49:,.0f} ns")


if __name__ == "__main__":
    bench_solve_raw_overhead()
    bench_per_path_overhead()
    bench_batch_overhead()
    bench_cache_solve_vs_solve_raw()
    bench_result_construction()
    bench_python_list_overhead()
