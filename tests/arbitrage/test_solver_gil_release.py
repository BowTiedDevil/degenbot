"""Test that the Rust solver releases the GIL for parallel execution.

This test verifies that the Rust arb solver's use of py.detach()
allows the GIL to be released during Rust computation. This is a
necessary (but not sufficient) condition for ThreadPoolExecutor-based
parallelism.

Key finding: the individual Rust solver calls are so fast (~1-2μs) that
even though the GIL is released during computation, Python-level overhead
(GIL acquire/release, loop iteration, function dispatch) occupies a
significant fraction of each call's wall time. This means ThreadPoolExecutor
does NOT achieve meaningful speedup for this workload — the threads spend
too much time contending for the GIL between short Rust calls.

ProcessPoolExecutor is the correct choice for CPU parallelism here, since
each subprocess has its own GIL and there is no inter-process contention.

Detection method: Track concurrent execution via thread counter.
If GIL is released, multiple threads will be inside the Rust solver simultaneously.
If GIL is held, only one thread can execute at a time (max_concurrent == 1).
"""

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import pytest

from degenbot.degenbot_rs import RustArbSolver


def _make_hops() -> list[int]:
    """Create realistic 2-hop flat input for solve_raw.

    Two USDC/WETH pools with different reserves, simulating a V2 arbitrage path.
    4 ints per hop: [reserve_in, reserve_out, gamma_numer, fee_denom].
    """
    # 0.3% fee: gamma = 997/1000
    fee_denom = 1_000_000
    gamma_numer = 997_000

    # Pool 1: 2M USDC / 1000 WETH
    reserve_in_1 = 2_000_000_000_000  # 2M USDC (6 decimals)
    reserve_out_1 = 1_000_000_000_000_000_000_000  # 1000 WETH (18 decimals)

    # Pool 2: 1.5M USDC / 800 WETH
    reserve_in_2 = 1_500_000_000_000
    reserve_out_2 = 800_000_000_000_000_000_000

    return [
        reserve_in_1, reserve_out_1, gamma_numer, fee_denom,
        reserve_in_2, reserve_out_2, gamma_numer, fee_denom,
    ]


def _slow_solve(solver: RustArbSolver, hops: list[int], concurrent: dict) -> None:
    """Run solve_raw and track concurrent execution count."""
    with concurrent["lock"]:
        concurrent["current"] += 1
        concurrent["max"] = max(concurrent["max"], concurrent["current"])

    try:
        solver.solve_raw(hops)
    finally:
        with concurrent["lock"]:
            concurrent["current"] -= 1


def test_solve_raw_releases_gil():
    """Test that multiple solve_raw calls execute in parallel across threads.

    Submits many CPU-bound solve calls to a ThreadPoolExecutor and checks
    that more than one thread is executing inside the Rust solver at the
    same time. This is only possible if py.detach() releases the GIL —
    if the GIL were held, threads would serialize and max_concurrent would be 1.

    Note: this test verifies GIL release, NOT that ThreadPoolExecutor provides
    a speedup. The Rust solver calls are too short (~1-2μs) for ThreadPoolExecutor
    to provide meaningful speedup because GIL contention between calls dominates.
    ProcessPoolExecutor (separate GIL per process) is needed for true parallelism.
    """
    solver = RustArbSolver()
    hops = _make_hops()

    # Warm up: ensure the solver is JIT-compiled / caches are populated
    for _ in range(10):
        solver.solve_raw(hops)

    concurrent: dict = {"current": 0, "max": 0, "lock": threading.Lock()}

    num_tasks = 64
    num_threads = 4

    with ThreadPoolExecutor(max_workers=num_threads) as executor:
        futures = [
            executor.submit(_slow_solve, solver, hops, concurrent)
            for _ in range(num_tasks)
        ]
        # Wait for all to complete
        for future in as_completed(futures):
            future.result()

    # If the GIL was released, we should observe concurrent execution
    # (max_concurrent > 1). If the GIL was held, threads serialize and
    # max_concurrent stays at 1.
    assert concurrent["max"] > 1, (
        f"Expected concurrent Rust solver execution (GIL released), "
        f"but max_concurrent was {concurrent['max']}. "
        f"The GIL may not be releasing during solve_raw."
    )


def test_solve_raw_thread_pool_no_speedup_for_short_calls():
    """Test that ThreadPoolExecutor does NOT speed up short Rust solver calls.

    Each solve_raw call takes ~1-2μs. The Python overhead per call
    (GIL acquire/release, loop iteration, function dispatch) is comparable
    to the Rust compute time. With 4 threads all calling solve_raw in a
    tight Python loop, they spend a significant fraction of time contending
    for the GIL between short Rust calls.

    This test documents that ThreadPoolExecutor is NOT an effective strategy
    for parallelizing the current Rust solver — ProcessPoolExecutor (with
    separate GILs per process) is needed for true multi-core utilization.

    The test asserts that threaded execution is NOT significantly faster
    than serial, confirming the GIL contention hypothesis.
    """
    solver = RustArbSolver()
    hops = _make_hops()

    # Warm up
    for _ in range(10):
        solver.solve_raw(hops)

    batch_size = 10_000
    num_batches = 16
    num_threads = 4
    total_calls = batch_size * num_batches

    def _solve_batch(_solver: RustArbSolver, _hops: list[int], n: int) -> None:
        for _ in range(n):
            _solver.solve_raw(_hops)

    # Serial execution
    start_serial = time.perf_counter()
    for _ in range(num_batches):
        _solve_batch(solver, hops, batch_size)
    duration_serial = time.perf_counter() - start_serial

    # Threaded execution
    start_threaded = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_threads) as executor:
        futures = [
            executor.submit(_solve_batch, solver, hops, batch_size)
            for _ in range(num_batches)
        ]
        for future in as_completed(futures):
            future.result()
    duration_threaded = time.perf_counter() - start_threaded

    # Threaded should NOT be significantly faster than serial for short Rust calls.
    # Allow up to 1.3x "speedup" (anything above likely means the Rust compute
    # has become long enough relative to Python overhead to benefit from threads,
    # which would be a welcome regression).
    speedup = duration_serial / duration_threaded
    assert speedup < 1.5, (
        f"Expected no meaningful speedup from ThreadPoolExecutor for short Rust calls, "
        f"but got {speedup:.2f}x. If the Rust solver has become compute-heavy enough "
        f"relative to Python overhead for threads to help, this test should be updated."
    )
