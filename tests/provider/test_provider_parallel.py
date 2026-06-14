"""Test that provider methods release the GIL for parallel execution.

This test verifies that the Rust provider's use of py.detach()
releases the GIL, allowing concurrent RPC execution across multiple
Python threads.

The AlloyProvider.verify_gil_release() method provides a deterministic,
timing-free proof: it spawns an OS thread inside py.detach() that calls
Python::attach(). If the GIL was actually released, the spawned thread can
re-acquire it, and the method returns True. If the GIL was not released,
the spawned thread would deadlock waiting for it, and the method would
never return (or return False).

This is a necessary (but not sufficient) condition for ThreadPoolExecutor-based
parallelism.
"""

from degenbot.degenbot_rs import AlloyProvider

HTTP_RPC_URL = "https://ethereum.publicnode.com"


def test_provider_releases_gil():
    """Test that py.detach() releases the GIL during Rust provider execution.

    Uses AlloyProvider.verify_gil_release() which deterministically proves
    GIL release by spawning an OS thread inside py.detach() that attempts
    Python::attach(). No timing assumptions, no thread counts, no
    probabilistic assertions.
    """
    provider = AlloyProvider(HTTP_RPC_URL, max_retries=3)
    assert provider.verify_gil_release(), (
        "GIL was not released during py.detach(). "
        "The Rust provider cannot run in parallel across Python threads."
    )
