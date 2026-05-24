"""Benchmark: ThreadPoolExecutor dispatching to Rust arbitrage solvers.

The hypothesis: Python-side data preparation (building SolveInput,
constructing RustIntHopState objects, etc.) dominates the total time,
swamping the actual Rust compute. Since the GIL is released during
Rust computation but held during data prep, ThreadPoolExecutor workers
serialize on the prep phase and don't achieve real parallelism.

This benchmark measures:
1. Pure Rust solve latency (no Python overhead)
2. Python-side data prep latency for each solve path
3. Total wall time for N solves via ThreadPoolExecutor
4. Breakdown of where time is spent via cProfile

Three solve paths are tested:
- solve_raw: flat Python int list → Rust
- solve_cached: pool IDs → Rust (pre-registered)
- solve (via MobiusSolver): RustIntHopState Python objects → Rust
"""

import cProfile
import pstats
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from fractions import Fraction

from degenbot.arbitrage.optimizers.hop_types import SolveInput, SolveResult
from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.degenbot_rs import RustArbSolver, RustIntHopState, RustPoolCache
from degenbot.types.hop_types import ConstantProductHop

# ==============================================================================
# Realistic reserve magnitudes (same as conftest.py)
# ==============================================================================

USDC_DECIMALS = 6
WETH_DECIMALS = 18

USDC_2M = 2_000_000 * 10**USDC_DECIMALS
USDC_1_5M = 1_500_000 * 10**USDC_DECIMALS
WETH_1000 = 1_000 * 10**WETH_DECIMALS
WETH_800 = 800 * 10**WETH_DECIMALS

FEE_0_3_PCT = Fraction(3, 1000)  # 0.3%

# Number of paths to simulate solving in parallel
NUM_PATHS = 500
# Number of worker threads
NUM_WORKERS = 8


def make_varied_paths(count: int) -> list[SolveInput]:
    """Create a list of varied 2-hop V2 arbitrage paths.

    Each path has slightly different reserves to prevent trivial caching
    and to simulate realistic heterogeneous work.
    """
    paths = []
    for i in range(count):
        # Vary reserves slightly around the base values
        factor = 1.0 + (i % 20) * 0.01  # ±10% variation
        r_in_0 = int(USDC_1_5M * factor)
        r_out_0 = int(WETH_800 * factor)
        r_in_1 = int(WETH_1000 * factor)
        r_out_1 = int(USDC_2M * factor)

        paths.append(
            SolveInput(
                hops=(
                    ConstantProductHop(
                        reserve_in=r_in_0,
                        reserve_out=r_out_0,
                        fee=FEE_0_3_PCT,
                    ),
                    ConstantProductHop(
                        reserve_in=r_in_1,
                        reserve_out=r_out_1,
                        fee=FEE_0_3_PCT,
                    ),
                )
            )
        )
    return paths


def make_varied_int_hops(count: int) -> list[list[int]]:
    """Create a list of flat int hop arrays for solve_raw.

    Each inner list has 4 ints per hop: [reserve_in, reserve_out, gamma_numer, fee_denom].
    """
    all_hops = []
    for i in range(count):
        factor = 1.0 + (i % 20) * 0.01
        r_in_0 = int(USDC_1_5M * factor)
        r_out_0 = int(WETH_800 * factor)
        r_in_1 = int(WETH_1000 * factor)
        r_out_1 = int(USDC_2M * factor)

        gamma_numer = FEE_0_3_PCT.denominator - FEE_0_3_PCT.numerator  # 997
        fee_denom = FEE_0_3_PCT.denominator  # 1000

        all_hops.append([
            r_in_0, r_out_0, gamma_numer, fee_denom,
            r_in_1, r_out_1, gamma_numer, fee_denom,
        ])
    return all_hops


def make_varied_rust_int_hop_states(count: int) -> list[list[RustIntHopState]]:
    """Create a list of RustIntHopState lists for the MobiusSolver solve path."""
    all_hops = []
    for i in range(count):
        factor = 1.0 + (i % 20) * 0.01
        r_in_0 = int(USDC_1_5M * factor)
        r_out_0 = int(WETH_800 * factor)
        r_in_1 = int(WETH_1000 * factor)
        r_out_1 = int(USDC_2M * factor)

        gamma_numer = FEE_0_3_PCT.denominator - FEE_0_3_PCT.numerator  # 997
        fee_denom = FEE_0_3_PCT.denominator  # 1000

        all_hops.append([
            RustIntHopState(r_in_0, r_out_0, gamma_numer, fee_denom),
            RustIntHopState(r_in_1, r_out_1, gamma_numer, fee_denom),
        ])
    return all_hops


def register_varied_pools(count: int, solver: ArbSolver) -> list[list[int]]:
    """Register varied pools in the solver's Rust cache.

    Returns a list of pool ID paths (each a list of 2 pool IDs).
    """
    paths = []
    for i in range(count):
        factor = 1.0 + (i % 20) * 0.01
        r_in = int(USDC_1_5M * factor)
        r_out = int(WETH_800 * factor)
        r_in_1 = int(WETH_1000 * factor)
        r_out_1 = int(USDC_2M * factor)

        pid0 = solver.register_pool(r_in, r_out, FEE_0_3_PCT)
        pid1 = solver.register_pool(r_in_1, r_out_1, FEE_0_3_PCT)
        paths.append([pid0, pid1])
    return paths


# ==============================================================================
# Benchmarks: Pure Rust latency (single-threaded, no Python overhead)
# ==============================================================================


def bench_pure_rust_solve_raw(iterations: int = 10_000) -> None:
    """Measure pure Rust solve_raw latency (flat int list input)."""
    solver = RustArbSolver()
    gamma_numer = 997
    fee_denom = 1000
    hops_flat = [
        USDC_1_5M, WETH_800, gamma_numer, fee_denom,
        WETH_1000, USDC_2M, gamma_numer, fee_denom,
    ]

    # Warmup
    for _ in range(100):
        solver.solve_raw(hops_flat, None)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve_raw(hops_flat, None)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  solve_raw: {per_call_ns:.0f} ns/call ({iterations} iterations)")


def bench_pure_rust_solve_cached(iterations: int = 10_000) -> None:
    """Measure pure Rust solve_cached latency (pool ID lookup)."""
    cache = RustPoolCache()
    gamma_numer = 997
    fee_denom = 1000
    cache.insert(1, USDC_1_5M, WETH_800, gamma_numer, fee_denom)
    cache.insert(2, WETH_1000, USDC_2M, gamma_numer, fee_denom)
    path = [1, 2]

    # Warmup
    for _ in range(100):
        cache.solve(path, None)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        cache.solve(path, None)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  solve_cached: {per_call_ns:.0f} ns/call ({iterations} iterations)")


def bench_pure_rust_solve_with_int_hop_states(iterations: int = 10_000) -> None:
    """Measure Rust solve via Python RustIntHopState objects."""
    solver = RustArbSolver()
    gamma_numer = 997
    fee_denom = 1000
    hops = [
        RustIntHopState(USDC_1_5M, WETH_800, gamma_numer, fee_denom),
        RustIntHopState(WETH_1000, USDC_2M, gamma_numer, fee_denom),
    ]

    # Warmup
    for _ in range(100):
        solver.solve(hops, None, None, 10)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve(hops, None, None, 10)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  solve (RustIntHopState): {per_call_ns:.0f} ns/call ({iterations} iterations)")


# ==============================================================================
# Benchmarks: Python-side data preparation cost
# ==============================================================================


def bench_python_data_prep_solve_input(iterations: int = 10_000) -> None:
    """Measure cost of building SolveInput from ConstantProductHop."""
    r_in_0 = USDC_1_5M
    r_out_0 = WETH_800
    r_in_1 = WETH_1000
    r_out_1 = USDC_2M
    fee = FEE_0_3_PCT

    # Warmup
    for _ in range(100):
        SolveInput(
            hops=(
                ConstantProductHop(reserve_in=r_in_0, reserve_out=r_out_0, fee=fee),
                ConstantProductHop(reserve_in=r_in_1, reserve_out=r_out_1, fee=fee),
            )
        )

    start = time.perf_counter_ns()
    for _ in range(iterations):
        SolveInput(
            hops=(
                ConstantProductHop(reserve_in=r_in_0, reserve_out=r_out_0, fee=fee),
                ConstantProductHop(reserve_in=r_in_1, reserve_out=r_out_1, fee=fee),
            )
        )
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  SolveInput construction: {per_call_ns:.0f} ns/call ({iterations} iterations)")


def bench_python_data_prep_rust_int_hop_state(iterations: int = 10_000) -> None:
    """Measure cost of constructing RustIntHopState Python objects."""
    gamma_numer = 997
    fee_denom = 1000

    # Warmup
    for _ in range(100):
        RustIntHopState(USDC_1_5M, WETH_800, gamma_numer, fee_denom)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        RustIntHopState(USDC_1_5M, WETH_800, gamma_numer, fee_denom)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  RustIntHopState construction: {per_call_ns:.0f} ns/call ({iterations} iterations)")


def bench_python_data_prep_solve_raw_flat_list(iterations: int = 10_000) -> None:
    """Measure cost of building the flat int list for solve_raw."""
    gamma_numer = 997
    fee_denom = 1000

    # Warmup
    for _ in range(100):
        [USDC_1_5M, WETH_800, gamma_numer, fee_denom,
         WETH_1000, USDC_2M, gamma_numer, fee_denom]

    start = time.perf_counter_ns()
    for _ in range(iterations):
        [USDC_1_5M, WETH_800, gamma_numer, fee_denom,
         WETH_1000, USDC_2M, gamma_numer, fee_denom]
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  Flat int list construction: {per_call_ns:.0f} ns/call ({iterations} iterations)")


# ==============================================================================
# Benchmarks: Full end-to-end through the PythonSolver → Rust path
# ==============================================================================


def bench_mobius_solver_end_to_end(iterations: int = 10_000) -> None:
    """Full MobiusSolver.solve() including Python dispatch overhead."""
    solver = MobiusSolver()
    solve_input = SolveInput(
        hops=(
            ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=FEE_0_3_PCT),
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=FEE_0_3_PCT),
        )
    )

    # Warmup
    for _ in range(100):
        solver.solve(solve_input)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve(solve_input)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  MobiusSolver.solve(): {per_call_ns:.0f} ns/call ({iterations} iterations)")


def bench_arb_solver_solve_cached(iterations: int = 10_000) -> None:
    """Full ArbSolver.solve_cached() including Python dispatch overhead."""
    solver = ArbSolver()
    gamma_numer = 997
    fee_denom = 1000
    pid0 = solver.register_pool(USDC_1_5M, WETH_800, FEE_0_3_PCT)
    pid1 = solver.register_pool(WETH_1000, USDC_2M, FEE_0_3_PCT)
    path = [pid0, pid1]

    # Warmup
    for _ in range(100):
        solver.solve_cached(path)

    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve_cached(path)
    elapsed = time.perf_counter_ns() - start

    per_call_ns = elapsed / iterations
    print(f"  ArbSolver.solve_cached(): {per_call_ns:.0f} ns/call ({iterations} iterations)")


# ==============================================================================
# Benchmarks: ThreadPoolExecutor with full queue
# ==============================================================================


def bench_threadpool_solve_raw(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with solve_raw (flat int list) path."""
    all_hops = make_varied_int_hops(num_paths)
    solver = RustArbSolver()

    def work(hops_flat: list[int]) -> bool:
        result = solver.solve_raw(hops_flat, None)
        return result.success

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_hops[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, h) for h in all_hops]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(solve_raw): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_threadpool_solve_cached(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with solve_cached (pool ID lookup) path."""
    solver = ArbSolver()
    paths = register_varied_pools(num_paths, solver)

    def work(path: list[int]) -> bool:
        try:
            result = solver.solve_cached(path)
            return result.profit > 0
        except Exception:
            return False

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, paths[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, p) for p in paths]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(solve_cached): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_threadpool_mobius_solver(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with MobiusSolver (RustIntHopState path)."""
    all_solve_inputs = make_varied_paths(num_paths)
    solver = MobiusSolver()

    def work(solve_input: SolveInput) -> bool:
        try:
            result = solver.solve(solve_input)
            return result.profit > 0
        except Exception:
            return False

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_solve_inputs[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, inp) for inp in all_solve_inputs]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(MobiusSolver): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


# ==============================================================================
# Benchmarks: ThreadPoolExecutor with on-the-fly data preparation
# ==============================================================================


def bench_threadpool_solve_raw_with_prep(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with solve_raw, data prep included in work item."""
    solver = RustArbSolver()
    fee = FEE_0_3_PCT
    gamma_numer = fee.denominator - fee.numerator
    fee_denom = fee.denominator

    # Raw ints that a pool would provide (simulating to_hop_state output)
    vary_factors = [1.0 + (i % 20) * 0.01 for i in range(num_paths)]
    raw_pool_data = []
    for factor in vary_factors:
        raw_pool_data.append((
            int(USDC_1_5M * factor), int(WETH_800 * factor),
            int(WETH_1000 * factor), int(USDC_2M * factor),
        ))

    def work(pool_data: tuple[int, int, int, int]) -> bool:
        r_in_0, r_out_0, r_in_1, r_out_1 = pool_data
        hops_flat = [r_in_0, r_out_0, gamma_numer, fee_denom,
                     r_in_1, r_out_1, gamma_numer, fee_denom]
        result = solver.solve_raw(hops_flat, None)
        return result.success

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, raw_pool_data[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, d) for d in raw_pool_data]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(solve_raw+prep): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_threadpool_mobius_with_prep(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with MobiusSolver, full Python data prep in work item.

    This simulates the real-world flow: each work item must construct
    ConstantProductHop, SolveInput, then call solver.solve().
    """
    solver = MobiusSolver()
    fee = FEE_0_3_PCT

    vary_factors = [1.0 + (i % 20) * 0.01 for i in range(num_paths)]
    raw_pool_data = []
    for factor in vary_factors:
        raw_pool_data.append((
            int(USDC_1_5M * factor), int(WETH_800 * factor),
            int(WETH_1000 * factor), int(USDC_2M * factor),
        ))

    def work(pool_data: tuple[int, int, int, int]) -> bool:
        r_in_0, r_out_0, r_in_1, r_out_1 = pool_data
        solve_input = SolveInput(
            hops=(
                ConstantProductHop(reserve_in=r_in_0, reserve_out=r_out_0, fee=fee),
                ConstantProductHop(reserve_in=r_in_1, reserve_out=r_out_1, fee=fee),
            )
        )
        try:
            result = solver.solve(solve_input)
            return result.profit > 0
        except Exception:
            return False

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, raw_pool_data[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, d) for d in raw_pool_data]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(MobiusSolver+prep): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_threadpool_solve_cached_with_prep(
    num_paths: int = NUM_PATHS,
    num_workers: int = NUM_WORKERS,
) -> None:
    """Benchmark: ThreadPoolExecutor with solve_cached, path assembly in work item.

    This simulates: pools are pre-registered in cache, but the path
    (list of pool IDs) is assembled per work item.
    """
    solver = ArbSolver()
    paths = register_varied_pools(num_paths, solver)

    def work(path: list[int]) -> bool:
        try:
            result = solver.solve_cached(path)
            return result.profit > 0
        except Exception:
            return False

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, paths[:num_workers]))

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, p) for p in paths]
        results = [f.result() for f in as_completed(futures)]
    elapsed = time.perf_counter() - start

    successful = sum(1 for r in results if r)
    print(f"  ThreadPool(solve_cached+path): {num_paths} paths, {num_workers} workers")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


# ==============================================================================
# Scaling test: 1, 2, 4, 8 workers
# ==============================================================================


def bench_threadpool_scaling(
    num_paths: int = NUM_PATHS,
) -> None:
    """Measure how throughput scales with worker count for the cheapest path."""
    solver = ArbSolver()
    paths = register_varied_pools(num_paths, solver)

    for num_workers in [1, 2, 4, 8]:
        def work(path: list[int]) -> bool:
            try:
                result = solver.solve_cached(path)
                return result.profit > 0
            except Exception:
                return False

        # Warmup
        with ThreadPoolExecutor(max_workers=num_workers) as executor:
            list(executor.map(work, paths[:num_workers]))

        start = time.perf_counter()
        with ThreadPoolExecutor(max_workers=num_workers) as executor:
            futures = [executor.submit(work, p) for p in paths]
            results = [f.result() for f in as_completed(futures)]
        elapsed = time.perf_counter() - start

        successful = sum(1 for r in results if r)
        throughput = num_paths / elapsed
        print(f"  {num_workers} workers: {elapsed*1000:.1f} ms, "
              f"{throughput:.0f} paths/s, "
              f"{elapsed/num_paths*1e6:.1f} μs/path, "
              f"{successful}/{num_paths} ok")


# ==============================================================================
# Profiled run: cProfile to identify bottlenecks
# ==============================================================================


def profile_threadpool_mobius_solver(
    num_paths: int = 200,
    num_workers: int = 4,
) -> None:
    """Profile MobiusSolver + ThreadPoolExecutor to find bottleneck functions."""
    all_solve_inputs = make_varied_paths(num_paths)
    solver = MobiusSolver()

    def work(solve_input: SolveInput) -> bool:
        try:
            result = solver.solve(solve_input)
            return result.profit > 0
        except Exception:
            return False

    profiler = cProfile.Profile()

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_solve_inputs[:num_workers]))

    profiler.enable()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, inp) for inp in all_solve_inputs]
        results = [f.result() for f in as_completed(futures)]
    profiler.disable()

    successful = sum(1 for r in results if r)
    print(f"  Profiled ThreadPool(MobiusSolver): {num_paths} paths, {num_workers} workers")
    print(f"  Successful: {successful}/{num_paths}")
    print()

    stats = pstats.Stats(profiler)
    stats.sort_stats(pstats.SortKey.CUMULATIVE)
    print("  Top 30 by cumulative time:")
    stats.print_stats(30)

    stats.sort_stats(pstats.SortKey.TIME)
    print("\n  Top 30 by self time:")
    stats.print_stats(30)


def profile_threadpool_solve_raw(
    num_paths: int = 200,
    num_workers: int = 4,
) -> None:
    """Profile solve_raw + ThreadPoolExecutor to find bottleneck functions."""
    all_hops = make_varied_int_hops(num_paths)
    solver = RustArbSolver()

    def work(hops_flat: list[int]) -> bool:
        result = solver.solve_raw(hops_flat, None)
        return result.success

    profiler = cProfile.Profile()

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_hops[:num_workers]))

    profiler.enable()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, h) for h in all_hops]
        results = [f.result() for f in as_completed(futures)]
    profiler.disable()

    successful = sum(1 for r in results if r)
    print(f"  Profiled ThreadPool(solve_raw): {num_paths} paths, {num_workers} workers")
    print(f"  Successful: {successful}/{num_paths}")
    print()

    stats = pstats.Stats(profiler)
    stats.sort_stats(pstats.SortKey.CUMULATIVE)
    print("  Top 30 by cumulative time:")
    stats.print_stats(30)

    stats.sort_stats(pstats.SortKey.TIME)
    print("\n  Top 30 by self time:")
    stats.print_stats(30)


def profile_threadpool_solve_cached(
    num_paths: int = 200,
    num_workers: int = 4,
) -> None:
    """Profile solve_cached + ThreadPoolExecutor to find bottleneck functions."""
    solver = ArbSolver()
    paths = register_varied_pools(num_paths, solver)

    def work(path: list[int]) -> bool:
        try:
            result = solver.solve_cached(path)
            return result.profit > 0
        except Exception:
            return False

    profiler = cProfile.Profile()

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, paths[:num_workers]))

    profiler.enable()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, p) for p in paths]
        results = [f.result() for f in as_completed(futures)]
    profiler.disable()

    successful = sum(1 for r in results if r)
    print(f"  Profiled ThreadPool(solve_cached): {num_paths} paths, {num_workers} workers")
    print(f"  Successful: {successful}/{num_paths}")
    print()

    stats = pstats.Stats(profiler)
    stats.sort_stats(pstats.SortKey.CUMULATIVE)
    print("  Top 30 by cumulative time:")
    stats.print_stats(30)

    stats.sort_stats(pstats.SortKey.TIME)
    print("\n  Top 30 by self time:")
    stats.print_stats(30)


# ==============================================================================
# Sequential baseline (no threading)
# ==============================================================================


def bench_sequential_solve_cached(num_paths: int = NUM_PATHS) -> None:
    """Sequential baseline for solve_cached."""
    solver = ArbSolver()
    paths = register_varied_pools(num_paths, solver)

    start = time.perf_counter()
    successful = 0
    for path in paths:
        try:
            result = solver.solve_cached(path)
            if result.profit > 0:
                successful += 1
        except Exception:
            pass
    elapsed = time.perf_counter() - start

    print(f"  Sequential(solve_cached): {num_paths} paths")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_sequential_solve_raw(num_paths: int = NUM_PATHS) -> None:
    """Sequential baseline for solve_raw."""
    all_hops = make_varied_int_hops(num_paths)
    solver = RustArbSolver()

    start = time.perf_counter()
    successful = 0
    for hops in all_hops:
        result = solver.solve_raw(hops, None)
        if result.success:
            successful += 1
    elapsed = time.perf_counter() - start

    print(f"  Sequential(solve_raw): {num_paths} paths")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


def bench_sequential_mobius_solver(num_paths: int = NUM_PATHS) -> None:
    """Sequential baseline for MobiusSolver."""
    all_solve_inputs = make_varied_paths(num_paths)
    solver = MobiusSolver()

    start = time.perf_counter()
    successful = 0
    for inp in all_solve_inputs:
        try:
            result = solver.solve(inp)
            if result.profit > 0:
                successful += 1
        except Exception:
            pass
    elapsed = time.perf_counter() - start

    print(f"  Sequential(MobiusSolver): {num_paths} paths")
    print(f"    Wall time: {elapsed*1000:.1f} ms ({elapsed/num_paths*1e6:.1f} μs/path)")
    print(f"    Successful: {successful}/{num_paths}")


# ==============================================================================
# GIL contention test: measure how much time threads spend waiting
# ==============================================================================


def bench_gil_contention(num_paths: int = NUM_PATHS, num_workers: int = NUM_WORKERS) -> None:
    """Measure GIL contention by comparing thread CPU time vs wall time."""
    import threading

    all_hops = make_varied_int_hops(num_paths)
    solver = RustArbSolver()

    # Track per-thread CPU time
    thread_cpu_times: dict[int, float] = {}
    lock = threading.Lock()

    def work(hops_flat: list[int]) -> bool:
        tid = threading.get_ident()
        start_cpu = time.thread_time_ns()
        result = solver.solve_raw(hops_flat, None)
        elapsed_cpu = time.thread_time_ns() - start_cpu
        with lock:
            thread_cpu_times[tid] = thread_cpu_times.get(tid, 0) + elapsed_cpu
        return result.success

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_hops[:num_workers]))

    thread_cpu_times.clear()

    wall_start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(work, h) for h in all_hops]
        results = [f.result() for f in as_completed(futures)]
    wall_elapsed = time.perf_counter() - wall_start

    total_cpu = sum(thread_cpu_times.values()) / 1e9
    successful = sum(1 for r in results if r)
    parallelism = total_cpu / wall_elapsed if wall_elapsed > 0 else 0

    print(f"  GIL contention test (solve_raw), {num_paths} paths, {num_workers} workers")
    print(f"    Wall time:  {wall_elapsed*1000:.1f} ms")
    print(f"    Total CPU:  {total_cpu*1000:.1f} ms")
    print(f"    Parallelism: {parallelism:.2f}x (ideal: {num_workers}x)")
    print(f"    CPU utilization: {parallelism/num_workers*100:.0f}%")
    print(f"    Successful: {successful}/{num_paths}")


# ==============================================================================
# Main
# ==============================================================================


def main() -> None:
    print("=" * 80)
    print("ThreadPool + Rust Solver Performance Benchmark")
    print("=" * 80)

    print("\n--- Phase 1: Pure Rust latency (single-threaded, no Python overhead) ---")
    bench_pure_rust_solve_raw()
    bench_pure_rust_solve_cached()
    bench_pure_rust_solve_with_int_hop_states()

    print("\n--- Phase 2: Python-side data preparation cost ---")
    bench_python_data_prep_solve_input()
    bench_python_data_prep_rust_int_hop_state()
    bench_python_data_prep_solve_raw_flat_list()

    print("\n--- Phase 3: End-to-end solver latency (single-threaded) ---")
    bench_mobius_solver_end_to_end()
    bench_arb_solver_solve_cached()

    print("\n--- Phase 4: Sequential baselines ---")
    bench_sequential_solve_cached()
    bench_sequential_solve_raw()
    bench_sequential_mobius_solver()

    print("\n--- Phase 5: ThreadPoolExecutor (pre-built data) ---")
    bench_threadpool_solve_raw()
    bench_threadpool_solve_cached()
    bench_threadpool_mobius_solver()

    print("\n--- Phase 6: ThreadPoolExecutor (data prep in work item) ---")
    bench_threadpool_solve_raw_with_prep()
    bench_threadpool_mobius_with_prep()
    bench_threadpool_solve_cached_with_prep()

    print("\n--- Phase 7: ThreadPool scaling (1–8 workers) ---")
    bench_threadpool_scaling()

    print("\n--- Phase 8: GIL contention measurement ---")
    bench_gil_contention()

    print("\n--- Phase 9: cProfile profiling (solve_raw) ---")
    profile_threadpool_solve_raw()

    print("\n--- Phase 10: cProfile profiling (solve_cached) ---")
    profile_threadpool_solve_cached()

    print("\n--- Phase 11: cProfile profiling (MobiusSolver) ---")
    profile_threadpool_mobius_solver()


if __name__ == "__main__":
    main()
