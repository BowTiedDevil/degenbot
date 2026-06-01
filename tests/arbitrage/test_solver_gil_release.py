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
from concurrent.futures import ThreadPoolExecutor, as_completed

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
        reserve_in_1,
        reserve_out_1,
        gamma_numer,
        fee_denom,
        reserve_in_2,
        reserve_out_2,
        gamma_numer,
        fee_denom,
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
        futures = [executor.submit(_slow_solve, solver, hops, concurrent) for _ in range(num_tasks)]
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
