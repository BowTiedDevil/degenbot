"""Observing task-reap discipline for cancel-at-teardown paths."""

from __future__ import annotations

import asyncio
import contextlib


async def cancel_and_reap(task: asyncio.Future[object]) -> None:
    """Cancel ``task``, await its unwind, and OBSERVE the cancellation.

    The corpus discipline for teardown reaps: ``task.cancel()`` + ``await`` +
    ``assert cancelled()``. The awaited ``CancelledError`` is absorbed only
    once the cancel that caused it is this reap's own ``cancel()`` call; a
    task that ends any other way (absorbed cancellation, natural completion)
    fails the assertion instead of silently swallowing a lost cancellation.

    A task error is never suppressed — only the ``CancelledError`` the reap
    itself requested is absorbed, so a real failure still propagates to the
    caller.
    """
    task.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await task
    assert task.cancelled(), f"{task!r} ended without observing its cancellation"
