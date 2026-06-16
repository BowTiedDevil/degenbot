"""Test that the Rust solver releases the GIL for parallel execution."""

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any

from degenbot.degenbot_rs import mobius as rs_mobius


def _make_hops(repetitions: int = 1) -> list[int]:
    """Create realistic 2-hop flat input for solve_raw."""
    fee_denom = 1_000_000
    gamma_numer = 997_000
    reserve_in_1 = 2_000_000_000_000
    reserve_out_1 = 1_000_000_000_000_000_000_000
    reserve_in_2 = 1_500_000_000_000
    reserve_out_2 = 800_000_000_000_000_000_000
    base_hops = [
        reserve_in_1,
        reserve_out_1,
        gamma_numer,
        fee_denom,
        reserve_in_2,
        reserve_out_2,
        gamma_numer,
        fee_denom,
    ]
    return base_hops * repetitions


def _solve_with_counter(solver: Any, hops: list[int], concurrent: dict[str, Any]) -> None:
    """Run solve_raw and track concurrent execution count."""
    with concurrent["lock"]:
        concurrent["current"] += 1
        concurrent["max"] = max(concurrent["max"], concurrent["current"])

    try:
        solver.solve_raw(hops)
    finally:
        with concurrent["lock"]:
            concurrent["current"] -= 1


def test_solve_raw_releases_gil() -> None:
    """Multiple solve_raw calls can be inside Rust concurrently."""
    solver = rs_mobius.RustArbSolver()
    hops = _make_hops(repetitions=100_000)
    for _ in range(2):
        solver.solve_raw(hops)

    concurrent: dict[str, Any] = {"current": 0, "max": 0, "lock": threading.Lock()}

    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = [
            executor.submit(_solve_with_counter, solver, hops, concurrent) for _ in range(8)
        ]
        for future in as_completed(futures):
            future.result()

    assert concurrent["max"] > 1, (
        "Expected concurrent Rust solver execution; solve_raw may not be releasing the GIL."
    )


def test_solve_raw_thread_pool_no_speedup_for_short_calls() -> None:
    """Document that short solve_raw calls do not get useful ThreadPool speedup."""
    solver = rs_mobius.RustArbSolver()
    hops = _make_hops()
    for _ in range(10):
        solver.solve_raw(hops)

    batch_size = 10_000
    num_batches = 16

    def _solve_batch(_solver: Any, _hops: list[int], n: int) -> None:
        for _ in range(n):
            _solver.solve_raw(_hops)

    start_serial = time.perf_counter()
    for _ in range(num_batches):
        _solve_batch(solver, hops, batch_size)
    duration_serial = time.perf_counter() - start_serial

    start_threaded = time.perf_counter()
    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = [
            executor.submit(_solve_batch, solver, hops, batch_size) for _ in range(num_batches)
        ]
        for future in as_completed(futures):
            future.result()
    duration_threaded = time.perf_counter() - start_threaded

    speedup = duration_serial / duration_threaded
    assert speedup < 1.5, (
        "ThreadPoolExecutor unexpectedly sped up short Rust solver calls by "
        f"{speedup:.2f}x; update this test if the workload changed."
    )
