"""Test that the Rust solver releases the GIL for parallel execution.

This test verifies that py.detach() releases the GIL during Rust computation.
The RustArbSolver.verify_gil_release() method provides a deterministic,
timing-free proof: it spawns an OS thread inside py.detach() that calls
Python::attach(). If the GIL was actually released, the spawned thread can
re-acquire it, and the method returns True. If the GIL was not released,
the spawned thread would deadlock waiting for it, and the method would
never return (or return False).

This is a necessary (but not sufficient) condition for ThreadPoolExecutor-based
parallelism.

Key finding: the individual Rust solver calls are so fast (~1-2μs) that
even though the GIL is released during computation, Python-level overhead
(GIL acquire/release, loop iteration, function dispatch) occupies a
significant fraction of each call's wall time. This means ThreadPoolExecutor
does NOT achieve meaningful speedup for this workload — the threads spend
too much time contending for the GIL between short Rust calls.

ProcessPoolExecutor is the correct choice for CPU parallelism here, since
each subprocess has its own GIL and there is no inter-process contention.
"""

from degenbot.degenbot_rs import RustArbSolver


def test_solve_raw_releases_gil():
    """Test that py.detach() releases the GIL during Rust solver execution.

    Uses RustArbSolver.verify_gil_release() which deterministically proves
    GIL release by spawning an OS thread inside py.detach() that attempts
    Python::attach(). No timing assumptions, no thread counts, no
    probabilistic assertions.
    """
    solver = RustArbSolver()
    assert solver.verify_gil_release(), (
        "GIL was not released during py.detach(). "
        "The Rust solver cannot run in parallel across Python threads."
    )
