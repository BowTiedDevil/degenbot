"""The observing task-reap helper (the corpus cancel-at-teardown discipline)."""

from __future__ import annotations

import asyncio

import pytest

from degenbot.utils.tasks import cancel_and_reap


async def test_reaps_a_pending_task() -> None:
    """A pending task is cancelled, awaited, and observed cancelled."""
    task = asyncio.create_task(asyncio.sleep(3600))
    await cancel_and_reap(task)
    assert task.cancelled()


async def test_observes_an_absorbed_cancellation() -> None:
    """A task that swallows its cancel() fails the reap loudly — the
    cancellation is observed, never silently swallowed."""

    async def absorber() -> None:
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            return

    task = asyncio.create_task(absorber())
    await asyncio.sleep(0)  # start the body so the cancel lands mid-await
    with pytest.raises(AssertionError, match="cancellation"):
        await cancel_and_reap(task)
    assert task.done()
    assert not task.cancelled()


async def test_propagates_a_task_error() -> None:
    """A task error is never suppressed — only the reap's own cancellation."""
    boom = RuntimeError("loud abort")

    async def raiser() -> None:
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            raise boom from None

    task = asyncio.create_task(raiser())
    await asyncio.sleep(0)  # start the body so the cancel lands mid-await
    with pytest.raises(RuntimeError):
        await cancel_and_reap(task)
