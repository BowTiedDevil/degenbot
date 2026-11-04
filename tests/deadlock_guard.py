"""Deadlock-guard budgets for waits on completion the test cannot itself force.

A wait whose completion edge the test controls — an ``asyncio.Event`` a fake
sets, a task the test cancels, a pool thread that records its own start — is an
*ordering* gate: that edge is what the assertions read, and elapsed time plays
no part in them. The timeout wrapped around such a wait is **not** an
assertion. It exists only so a regression that never reaches the edge fails
fast with a named error instead of parking the worker until the suite timeout
fires.

The budgets below are therefore guards, not expectations. They sit far above
any plausible scheduling delay on a loaded runner (busy CI box, cold page
cache, a thread waiting its turn against the GIL). Tightening one converts a
correct-but-slow run into a clock-declared failure — exactly the flake this
module exists to prevent. Nothing here may sequence assertions: a test that
must prove "A happened before B" gates on an event it sets itself, never on a
duration.
"""

from __future__ import annotations

import threading
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Callable

# In-process waits: a task or thread a correct run reaches within milliseconds.
DEADLOCK_GUARD_S = 30.0

# Waits on an external process (anvil boot, a mined receipt): process startup
# and RPC round-trips dominate, so the guard is wider.
EXTERNAL_PROCESS_GUARD_S = 60.0


class Worker:
    """A thread whose completion is a signal the test waits on, not a duration.

    The body sets :attr:`finished` as it returns, so a test that needs "the
    worker is done" gates on that event — the ordering edge it can name — and
    reads :data:`DEADLOCK_GUARD_S` only as the budget in which a correct run
    reaches it. A regression that never reaches the edge fails with
    :meth:`wait_finished`'s named error instead of parking the worker, and the
    thread is a daemon so a fired guard leaves nothing holding the interpreter
    open.

    Use this instead of ``thread.join(timeout=…)`` followed by
    ``assert not thread.is_alive()``: there the timeout IS the assertion, which
    makes a correct-but-slow run a clock-declared deadlock.
    """

    def __init__(self, body: Callable[[], None], *, what: str) -> None:
        self._what = what
        self.finished = threading.Event()
        self._thread = threading.Thread(target=self._run, args=(body,), daemon=True)

    def _run(self, body: Callable[[], None]) -> None:
        try:
            body()
        finally:
            self.finished.set()

    def start(self) -> None:
        """Start the body on its own thread."""
        self._thread.start()

    def wait_finished(self) -> None:
        """Block until the body has returned, then reap the thread.

        The wait is bounded by :data:`DEADLOCK_GUARD_S` purely so a body that
        never returns names itself instead of hanging the worker.
        """
        assert self.finished.wait(DEADLOCK_GUARD_S), (
            f"{self._what} did not finish within the deadlock guard ({DEADLOCK_GUARD_S}s)"
        )
        self._thread.join()
