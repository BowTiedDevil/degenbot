"""Guard the module/session scoping invariant for AnvilFork fixtures.

A module-scoped AnvilFork must remain usable across *every* test in its module.
This regressed when the autouse per-test teardown called ``AnvilFork.close_all()``,
which indiscriminately reaps all live forks — including fixture-scoped ones
still in use — killing the Anvil process (and its IPC socket) between tests.
Subsequent ``.call()`` s then hit ``OSError: [Errno 9] Bad file descriptor``.

The safety net for *leaked* inline forks belongs at worker/session exit, not
between tests.
"""

from collections.abc import Generator

import pytest

from degenbot.anvil_fork import AnvilFork


@pytest.fixture(scope="module")
def shared_anvil() -> Generator[AnvilFork, None, None]:
    """A module-scoped standalone Anvil fork, mimicking ``standalone_anvil``."""
    fork = AnvilFork(
        fork_url=None,  # standalone
        ipc_provider_kwargs={"timeout": None},
    )
    yield fork
    fork.close()


def test_module_scoped_fork_alive_first(shared_anvil: AnvilFork):
    """First consumer: the fork responds to an RPC call."""
    assert shared_anvil.w3.is_connected()
    # Real RPC round-trip, not a cached attribute — dies if the socket was reaped.
    assert shared_anvil.w3.eth.chain_id == 31337


def test_module_scoped_fork_alive_after_autouse_teardown(shared_anvil: AnvilFork):
    """Second consumer: the *same* fork is still alive after the autouse
    between-test teardown ran following the previous test.

    If ``AnvilFork.close_all()`` is invoked per-test, it kills this module-scoped
    fork here and the IPC call raises ``OSError: [Errno 9] Bad file descriptor``.
    """
    assert shared_anvil.w3.is_connected()
    assert shared_anvil.w3.eth.chain_id == 31337
