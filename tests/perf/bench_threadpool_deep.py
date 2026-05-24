"""Deep investigation: Why ThreadPoolExecutor provides zero parallelism for Rust solver.

Phase 1 findings:
- Pure Rust solve: ~1μs
- Total end-to-end (MobiusSolver): ~3.5μs (3.5x Rust alone)
- ThreadPool with 8 workers: 1% CPU utilization, zero speedup
- Sequential is FASTER than ThreadPool

Hypotheses tested here:
1. The ~1μs Rust solve releases the GIL, but the surrounding Python
   dispatch code holds it, making GIL release ineffective.
2. ThreadPoolExecutor submit/schedule overhead exceeds the solve time.
3. Longer solves (3-hop, 4-hop) should benefit more from GIL release.
4. Batching many paths into a single Rust call amortizes the Python overhead.
5. The minimum GIL-hold time per work item is the bottleneck.
"""

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from fractions import Fraction

from degenbot.arbitrage.optimizers.hop_types import SolveInput
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.degenbot_rs import RustArbSolver, RustIntHopState, RustPoolCache
from degenbot.types.hop_types import ConstantProductHop

USDC_2M = 2_000_000 * 10**6
USDC_1_5M = 1_500_000 * 10**6
WETH_1000 = 1_000 * 10**18
WETH_800 = 800 * 10**18
FEE = Fraction(3, 1000)


# ==============================================================================
# Hypothesis 1: GIL hold time breakdown per work item
# ==============================================================================


def measure_gil_hold_time_per_path(num_paths: int = 1000) -> None:
    """Measure how much total time is spent holding the GIL vs not.

    Strategy: single thread runs all paths sequentially, measuring:
    - Time in Python code (GIL held)
    - Time in Rust solve_raw (GIL released via py.detach)
    """
    solver = RustArbSolver()
    gamma_numer = 997
    fee_denom = 1000

    all_hops = []
    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        all_hops.append([
            int(USDC_1_5M * factor), int(WETH_800 * factor), gamma_numer, fee_denom,
            int(WETH_1000 * factor), int(USDC_2M * factor), gamma_numer, fee_denom,
        ])

    # Measure just the Rust call (GIL released)
    start = time.perf_counter_ns()
    for hops in all_hops:
        solver.solve_raw(hops, None)
    rust_time_ns = time.perf_counter_ns() - start

    # Measure Rust call + list construction (Python overhead, GIL held)
    start = time.perf_counter_ns()
    for i in range(num_paths):
        hops = all_hops[i]
        solver.solve_raw(hops, None)
    total_time_ns = time.perf_counter_ns() - start

    python_overhead_ns = total_time_ns - rust_time_ns

    print("  GIL hold time breakdown (single-threaded, 2-hop):")
    print(f"    Rust solve_raw:  {rust_time_ns/num_paths:.0f} ns/path (GIL released)")
    print(f"    Total:           {total_time_ns/num_paths:.0f} ns/path")
    print(f"    Python overhead: {python_overhead_ns/num_paths:.0f} ns/path (GIL held)")
    print(f"    Python overhead: {python_overhead_ns/total_time_ns*100:.0f}% of total time")
    print(f"    GIL-held fraction: ~{total_time_ns/num_paths - rust_time_ns/num_paths:.0f} ns")


def measure_gil_hold_time_full_mobius(num_paths: int = 1000) -> None:
    """Same but through the full MobiusSolver → Rust path with SolveInput construction."""
    from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
    solver = MobiusSolver()
    fee = FEE

    # Pre-built Python objects (GIL held for construction)
    start = time.perf_counter_ns()
    all_inputs = []
    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        all_inputs.append(SolveInput(
            hops=(
                ConstantProductHop(
                    reserve_in=int(USDC_1_5M * factor),
                    reserve_out=int(WETH_800 * factor),
                    fee=fee,
                ),
                ConstantProductHop(
                    reserve_in=int(WETH_1000 * factor),
                    reserve_out=int(USDC_2M * factor),
                    fee=fee,
                ),
            )
        ))
    python_construction_ns = time.perf_counter_ns() - start

    # Just the solve call
    start = time.perf_counter_ns()
    for inp in all_inputs:
        solver.solve(inp)
    solve_time_ns = time.perf_counter_ns() - start

    # Full path: construction + solve
    start = time.perf_counter_ns()
    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        inp = SolveInput(
            hops=(
                ConstantProductHop(
                    reserve_in=int(USDC_1_5M * factor),
                    reserve_out=int(WETH_800 * factor),
                    fee=fee,
                ),
                ConstantProductHop(
                    reserve_in=int(WETH_1000 * factor),
                    reserve_out=int(USDC_2M * factor),
                    fee=fee,
                ),
            )
        )
        solver.solve(inp)
    full_time_ns = time.perf_counter_ns() - start

    print("  GIL hold time breakdown (full MobiusSolver path, 2-hop):")
    print(f"    Python construction: {python_construction_ns/num_paths:.0f} ns/path (GIL held)")
    print(f"    Solve only:         {solve_time_ns/num_paths:.0f} ns/path (mixed GIL)")
    print(f"    Full path:          {full_time_ns/num_paths:.0f} ns/path")
    print(f"    Construction:      {python_construction_ns/full_time_ns*100:.0f}% of total")


# ==============================================================================
# Hypothesis 2: ThreadPool overhead for 1μs work items
# ==============================================================================


def measure_threadpool_overhead(num_paths: int = 500, num_workers: int = 8) -> None:
    """Measure ThreadPoolExecutor submit + schedule + collect overhead.

    Uses a no-op work function to measure bare overhead.
    """
    def noop_work(_: int) -> bool:
        return True

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(noop_work, range(num_workers)))

    # ThreadPool submission + collection overhead
    start = time.perf_counter_ns()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        futures = [executor.submit(noop_work, i) for i in range(num_paths)]
        results = [f.result() for f in as_completed(futures)]
    pool_ns = time.perf_counter_ns() - start

    print(f"  ThreadPool overhead ({num_paths} items, {num_workers} workers):")
    print(f"    ThreadPool no-op: {pool_ns/num_paths:.0f} ns/item")
    print(f"    Overhead / Rust solve time: ~{pool_ns/num_paths / 1000:.1f}x")
    print(f"    Overhead / Rust solve time: ~{pool_ns/num_paths / 1100:.1f}x (at 1100ns Rust solve)")


# ==============================================================================
# Hypothesis 3: Longer solves should benefit more from GIL release
# ==============================================================================


def build_n_hop_path(n_hops: int, base_factor: float = 1.0) -> list[int]:
    """Build a flat int list for an n-hop path with slight variation per hop."""
    gamma_numer = 997
    fee_denom = 1000
    hops = []
    for i in range(n_hops):
        factor = base_factor * (1.0 + (i % 5) * 0.02)
        if i % 2 == 0:
            hops.extend([int(USDC_1_5M * factor), int(WETH_800 * factor), gamma_numer, fee_denom])
        else:
            hops.extend([int(WETH_1000 * factor), int(USDC_2M * factor), gamma_numer, fee_denom])
    return hops


def bench_solve_length_vs_parallelism() -> None:
    """Test whether longer solve paths (3-hop, 4-hop, etc.) allow more parallelism."""
    solver = RustArbSolver()
    num_paths = 500

    for n_hops in [2, 3, 4, 6, 8]:
        all_hops = [build_n_hop_path(n_hops, 1.0 + (i % 20) * 0.01) for i in range(num_paths)]

        # Sequential baseline
        start = time.perf_counter()
        for hops in all_hops:
            solver.solve_raw(hops, None)
        seq_elapsed = time.perf_counter() - start

        # ThreadPool with 8 workers
        def work(h: list[int]) -> bool:
            r = solver.solve_raw(h, None)
            return r.success

        with ThreadPoolExecutor(max_workers=8) as executor:
            futures = [executor.submit(work, h) for h in all_hops]
            results = [f.result() for f in as_completed(futures)]
        pool_elapsed = time.perf_counter() - start

        speedup = seq_elapsed / pool_elapsed if pool_elapsed > 0 else 0
        seq_per = seq_elapsed / num_paths * 1e6
        pool_per = pool_elapsed / num_paths * 1e6

        print(f"  {n_hops}-hop: seq={seq_per:.1f}μs/path, pool={pool_per:.1f}μs/path, "
              f"speedup={speedup:.2f}x")


# ==============================================================================
# Hypothesis 4: Batched solve (many paths in one Rust call) amortizes GIL overhead
# ==============================================================================


def bench_batched_solve(num_paths: int = 500) -> None:
    """Measure batched solve via RustPoolCache (register + solve_cached)."""
    solver = ArbSolver()

    # Register all pools
    paths = []
    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        pid0 = solver.register_pool(
            int(USDC_1_5M * factor), int(WETH_800 * factor), FEE
        )
        pid1 = solver.register_pool(
            int(WETH_1000 * factor), int(USDC_2M * factor), FEE
        )
        paths.append([pid0, pid1])

    # Sequential: solve_cached one at a time
    start = time.perf_counter()
    successful = 0
    for path in paths:
        try:
            result = solver.solve_cached(path)
            if result.profit > 0:
                successful += 1
        except Exception:
            pass
    seq_elapsed = time.perf_counter() - start

    # ThreadPool: solve_cached with 8 workers
    def work(path: list[int]) -> bool:
        try:
            result = solver.solve_cached(path)
            return result.profit > 0
        except Exception:
            return False

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=8) as executor:
        futures = [executor.submit(work, p) for p in paths]
        results = [f.result() for f in as_completed(futures)]
    pool_elapsed = time.perf_counter() - start

    speedup = seq_elapsed / pool_elapsed if pool_elapsed > 0 else 0

    print(f"  Sequential solve_cached: {seq_elapsed/num_paths*1e6:.1f}μs/path ({successful} ok)")
    print(f"  ThreadPool solve_cached: {pool_elapsed/num_paths*1e6:.1f}μs/path")
    print(f"  Speedup: {speedup:.2f}x")


# ==============================================================================
# Hypothesis 5: GIL context-switch overhead dominates for short work items
# ==============================================================================


def measure_gil_switch_cost() -> None:
    """Estimate the cost of a single GIL acquire/release cycle.

    A thread that repeatedly acquires/releases the GIL without doing any
    work should show the minimum overhead per GIL cycle.
    """
    iterations = 100_000

    # Measure how fast a single thread can do GIL-bound work
    start = time.perf_counter_ns()
    x = 0
    for _ in range(iterations):
        x += 1  # Pure Python work (must hold GIL)
    single_thread_ns = time.perf_counter_ns() - start

    # Now two threads competing for GIL doing the same work
    def count_up(n: int) -> int:
        x = 0
        for _ in range(n):
            x += 1
        return x

    start = time.perf_counter_ns()
    with ThreadPoolExecutor(max_workers=2) as executor:
        f1 = executor.submit(count_up, iterations)
        f2 = executor.submit(count_up, iterations)
        f1.result()
        f2.result()
    two_thread_ns = time.perf_counter_ns() - start

    single_per_iter = single_thread_ns / iterations
    two_per_iter = two_thread_ns / (2 * iterations)  # 2 threads * iterations
    gil_contention_overhead = two_per_iter - single_per_iter

    print("  GIL context-switch cost estimation:")
    print(f"    Single-thread: {single_per_iter:.1f} ns/iteration")
    print(f"    Two threads:   {two_per_iter:.1f} ns/iteration (per thread)")
    print(f"    GIL contention: {gil_contention_overhead:.1f} ns/iteration")
    print(f"    Slowdown: {two_per_iter/single_per_iter:.2f}x")
    print()
    print(f"  Rust solve_raw: ~1100 ns/call")
    print(f"  GIL contention overhead: ~{gil_contention_overhead:.0f} ns/call")
    print(f"  GIL overhead as % of Rust solve: {gil_contention_overhead/1100*100:.0f}%")


# ==============================================================================
# Detailed timing: what happens inside a single MobiusSolver.solve() call
# ==============================================================================


def breakdown_mobius_solve(iterations: int = 10_000) -> None:
    """Break down the time spent in each phase of MobiusSolver.solve()."""
    from degenbot.arbitrage.optimizers.mobius_solver import MobiusSolver
    solver = MobiusSolver()
    fee = FEE

    solve_input = SolveInput(
        hops=(
            ConstantProductHop(reserve_in=USDC_1_5M, reserve_out=WETH_800, fee=fee),
            ConstantProductHop(reserve_in=WETH_1000, reserve_out=USDC_2M, fee=fee),
        )
    )

    # Phase 1: time.perf_counter_ns() + dispatch into solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        start_ns = time.perf_counter_ns()
    timing_overhead = time.perf_counter_ns() - start

    # Phase 2: _build_solve_input equivalent (just accessing pre-built input)
    start = time.perf_counter_ns()
    for _ in range(iterations):
        _ = solve_input.hops
        _ = solve_input.max_input
    access_overhead = time.perf_counter_ns() - start

    # Phase 3: Full solve
    start = time.perf_counter_ns()
    for _ in range(iterations):
        solver.solve(solve_input)
    total_ns = time.perf_counter_ns() - start

    # Phase 4: Just the Rust _try_rust_solve_raw internals
    # (We can't easily separate these, but we can measure RustArbSolver.solve_raw directly)
    gamma_numer = fee.denominator - fee.numerator
    fee_denom = fee.denominator
    rust_solver = RustArbSolver()
    hops_flat = [USDC_1_5M, WETH_800, gamma_numer, fee_denom,
                 WETH_1000, USDC_2M, gamma_numer, fee_denom]

    start = time.perf_counter_ns()
    for _ in range(iterations):
        rust_solver.solve_raw(hops_flat, None)
    rust_ns = time.perf_counter_ns() - start

    # Phase 5: int_hops_flat construction from solve_input
    start = time.perf_counter_ns()
    for _ in range(iterations):
        int_hops_flat = []
        for hop in solve_input.hops:
            fee_denom_h = hop.fee.denominator
            gamma_numer_h = fee_denom_h - hop.fee.numerator
            int_hops_flat.extend([hop.reserve_in, hop.reserve_out, gamma_numer_h, fee_denom_h])
    prep_ns = time.perf_counter_ns() - start

    python_overhead = total_ns - rust_ns

    print("  MobiusSolver.solve() breakdown:")
    print(f"    Total (MobiusSolver.solve):  {total_ns/iterations:.0f} ns")
    print(f"    Rust (solve_raw):            {rust_ns/iterations:.0f} ns ({rust_ns/total_ns*100:.0f}%)")
    print(f"    Python overhead:             {python_overhead/iterations:.0f} ns ({python_overhead/total_ns*100:.0f}%)")
    print(f"    - int_hops_flat construction: {prep_ns/iterations:.0f} ns")
    print(f"    - timing + dispatch:          ~{(python_overhead - prep_ns)/iterations:.0f} ns (residual)")
    print()
    print("  GIL-held time estimate per solve:")
    print(f"    Python overhead:             {python_overhead/iterations:.0f} ns (GIL HELD)")
    print(f"    Rust solve_raw:              {rust_ns/iterations:.0f} ns (GIL RELEASED)")
    print(f"    GIL-held fraction:          {python_overhead/total_ns*100:.0f}%")
    print(f"    Effective GIL-held time:      {python_overhead/iterations:.0f} ns")
    print(f"    GIL-released time:           {rust_ns/iterations:.0f} ns")
    print()
    print("  For ThreadPoolExecutor to provide speedup:")
    print(f"    Need GIL-released time >> GIL-acquire/release overhead (~200ns)")
    print(f"    Current GIL-released time: {rust_ns/iterations:.0f} ns")
    print(f"    Ratio (GIL-released / GIL-switch): {rust_ns/iterations / 200:.1f}x")
    print(f"    Need ratio >> 1 for effective parallelism")


# ==============================================================================
# ThreadPoolExecutor: measure per-work-item overhead with piggyback timing
# ==============================================================================


def detailed_threadpool_timing(num_paths: int = 500, num_workers: int = 8) -> None:
    """Detailed timing of ThreadPoolExecutor internals.

    Measures:
    - Time to submit all futures
    - Time for first result
    - Time for last result
    - Per-path wall time from perspective of the work function
    """
    solver = RustArbSolver()
    gamma_numer = 997
    fee_denom = 1000

    all_hops = []
    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        all_hops.append([
            int(USDC_1_5M * factor), int(WETH_800 * factor), gamma_numer, fee_denom,
            int(WETH_1000 * factor), int(USDC_2M * factor), gamma_numer, fee_denom,
        ])

    # Per-work-function wall time tracking
    work_times: list[float] = []
    lock = threading.Lock()

    def work(hops_flat: list[int]) -> float:
        t0 = time.perf_counter_ns()
        result = solver.solve_raw(hops_flat, None)
        t1 = time.perf_counter_ns()
        elapsed = t1 - t0
        with lock:
            work_times.append(elapsed)
        return elapsed

    # Warmup
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        list(executor.map(work, all_hops[:num_workers]))
    work_times.clear()

    # Submit all futures
    submit_start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_workers) as executor:
        submit_end = time.perf_counter()
        futures = [executor.submit(work, h) for h in all_hops]
        all_submitted = time.perf_counter()

        results = [f.result() for f in as_completed(futures)]
    all_done = time.perf_counter()

    submit_time = all_submitted - submit_start
    total_wall = all_done - submit_start

    work_times.sort()
    median_work_ns = work_times[len(work_times) // 2]
    p99_work_ns = work_times[int(len(work_times) * 0.99)]
    max_work_ns = max(work_times)
    min_work_ns = min(work_times)
    avg_work_ns = sum(work_times) / len(work_times)

    print(f"  ThreadPoolExecutor detailed timing ({num_paths} paths, {num_workers} workers):")
    print(f"    Submit time:         {submit_time*1000:.1f} ms ({submit_time/num_paths*1e6:.1f} μs/submit)")
    print(f"    Total wall time:     {total_wall*1000:.1f} ms")
    print(f"    Wall time per path:  {total_wall/num_paths*1e6:.1f} μs")
    print()
    print(f"  Work function timing (time inside work()):")
    print(f"    Min:    {min_work_ns:.0f} ns")
    print(f"    Median: {median_work_ns:.0f} ns")
    print(f"    Avg:    {avg_work_ns:.0f} ns")
    print(f"    P99:    {p99_work_ns:.0f} ns")
    print(f"    Max:    {max_work_ns:.0f} ns")
    print()
    print(f"  Overhead analysis:")
    print(f"    Work fn median: {median_work_ns:.0f} ns")
    print(f"    Sequential Rust: ~1100 ns")
    print(f"    ThreadPool overhead per path: {total_wall/num_paths*1e9 - median_work_ns:.0f} ns")
    print(f"    ThreadPool overhead: {(total_wall/num_paths*1e9 - median_work_ns) / (total_wall/num_paths*1e9) * 100:.0f}% of wall time")


# ==============================================================================
# Key experiment: what is the minimum work duration for ThreadPool to help?
# ==============================================================================


def bench_minimum_useful_work_duration() -> None:
    """Find the minimum Rust solve duration where ThreadPoolExecutor provides speedup.

    Uses solve_cached with increasing numbers of hops (each solve
    takes longer). Measures when speedup > 1.0.
    """
    print("  Finding minimum work duration for ThreadPool speedup:")
    print()

    cache = RustPoolCache()
    # Register pools with varied IDs
    num_paths = 500
    paths_2hop = []
    paths_4hop = []
    paths_8hop = []

    for i in range(num_paths):
        factor = 1.0 + (i % 20) * 0.01
        r_in = int(USDC_1_5M * factor)
        r_out = int(WETH_800 * factor)
        r_in_1 = int(WETH_1000 * factor)
        r_out_1 = int(USDC_2M * factor)

        base_id = i * 20
        cache.insert(base_id, r_in, r_out, 997, 1000)
        cache.insert(base_id + 1, r_in_1, r_out_1, 997, 1000)
        paths_2hop.append([base_id, base_id + 1])

        # 4-hop: add intermediate pools
        cache.insert(base_id + 2, r_in, r_out, 997, 1000)
        cache.insert(base_id + 3, r_in_1, r_out_1, 997, 1000)
        paths_4hop.append([base_id, base_id + 1, base_id + 2, base_id + 3])

        # 8-hop
        for j in range(4, 8):
            cache.insert(base_id + j, r_in if j % 2 == 0 else r_in_1,
                         r_out if j % 2 == 0 else r_out_1, 997, 1000)
        paths_8hop.append([base_id + k for k in range(8)])

    for label, paths in [("2-hop", paths_2hop), ("4-hop", paths_4hop), ("8-hop", paths_8hop)]:
        def work(path: list[int]) -> bool:
            try:
                r = cache.solve(path, None)
                return r.success
            except Exception:
                return False

        # Sequential
        start = time.perf_counter()
        for p in paths:
            work(p)
        seq_elapsed = time.perf_counter() - start

        # ThreadPool with 8 workers
        with ThreadPoolExecutor(max_workers=8) as executor:
            futures = [executor.submit(work, p) for p in paths]
            results = [f.result() for f in as_completed(futures)]
        pool_elapsed = time.perf_counter() - start

        speedup = seq_elapsed / pool_elapsed if pool_elapsed > 0 else 0

        print(f"    {label:5s}: seq={seq_elapsed/num_paths*1e6:.1f}μs, "
              f"pool={pool_elapsed/num_paths*1e6:.1f}μs, "
              f"speedup={speedup:.2f}x")


# ==============================================================================
# Synthetic: spin in Rust for a controlled duration
# ==============================================================================


def bench_simulated_long_rust_work(num_paths: int = 500, num_workers: int = 8) -> None:
    """Simulate longer Rust work by doing repeated solve_raw calls per work item.

    This tests: if each work item takes 10μs in Rust, does ThreadPool help?
    """
    solver = RustArbSolver()
    gamma_numer = 997
    fee_denom = 1000
    hops_flat = [USDC_1_5M, WETH_800, gamma_numer, fee_denom,
                 WETH_1000, USDC_2M, gamma_numer, fee_denom]

    # Measure single Rust call duration
    start = time.perf_counter_ns()
    for _ in range(1000):
        solver.solve_raw(hops_flat, None)
    single_rust_ns = (time.perf_counter_ns() - start) / 1000

    repeats = [1, 5, 10, 20, 50, 100]

    print("  Simulated long Rust work (repeated solve_raw calls per item):")
    for repeat in repeats:
        expected_rust_ns = single_rust_ns * repeat

        def work(_: int, _repeat: int = repeat) -> bool:
            for _ in range(_repeat):
                solver.solve_raw(hops_flat, None)
            return True

        # Sequential
        start = time.perf_counter()
        for i in range(num_paths):
            work(i)
        seq_elapsed = time.perf_counter() - start

        # ThreadPool
        with ThreadPoolExecutor(max_workers=num_workers) as executor:
            futures = [executor.submit(work, i) for i in range(num_paths)]
            results = [f.result() for f in as_completed(futures)]
        pool_elapsed = time.perf_counter() - start

        speedup = seq_elapsed / pool_elapsed if pool_elapsed > 0 else 0
        print(f"    {repeat:3d}x ({expected_rust_ns/1000:.0f}μs Rust): "
              f"seq={seq_elapsed/num_paths*1e6:.1f}μs, "
              f"pool={pool_elapsed/num_paths*1e6:.1f}μs, "
              f"speedup={speedup:.2f}x")


# ==============================================================================
# Main
# ==============================================================================


def main() -> None:
    print("=" * 80)
    print("Deep Investigation: ThreadPool + Rust Solver Bottleneck")
    print("=" * 80)

    print("\n--- H1: GIL hold time breakdown per work item ---")
    measure_gil_hold_time_per_path()
    measure_gil_hold_time_full_mobius()

    print("\n--- H2: ThreadPool overhead for 1μs work items ---")
    measure_threadpool_overhead()

    print("\n--- H3: Solve path length vs parallelism ---")
    bench_solve_length_vs_parallelism()

    print("\n--- H4: Batched solve via RustPoolCache ---")
    bench_batched_solve()

    print("\n--- H5: GIL context-switch cost ---")
    measure_gil_switch_cost()

    print("\n--- MobiusSolver.solve() detailed breakdown ---")
    breakdown_mobius_solve()

    print("\n--- ThreadPoolExecutor detailed timing ---")
    detailed_threadpool_timing()

    print("\n--- Minimum useful work duration ---")
    bench_minimum_useful_work_duration()

    print("\n--- Simulated long Rust work (repeated solve_raw) ---")
    bench_simulated_long_rust_work()


if __name__ == "__main__":
    main()
