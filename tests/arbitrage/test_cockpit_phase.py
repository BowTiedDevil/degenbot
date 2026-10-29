"""Typed cockpit phase machine.

The driver cockpit enforces its startup ordering through a ``SessionPhase``
machine: New -> Started -> Running -> Closed. Illegal sequencing raises
:class:`~degenbot.runner.session.PhaseError` instead of failing obscurely, and
``shutdown()`` stays deliberately any-phase/idempotent (the SIGINT teardown
ordering depends on it).

Driven through the session lifecycle over the cockpit's one construction seam;
no anvil, no live RPC.
"""

from __future__ import annotations

import signal

import pytest

from degenbot.runner.session import PhaseError
from tests.helpers.session import session_for_test


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


class TestPhaseMachine:
    async def test_enqueue_path_before_run_raises_phase_error(self) -> None:
        async with session_for_test() as session:
            with pytest.raises(PhaseError):
                await session.enqueue_path([], directions=None)

    async def test_trigger_discovery_before_run_raises_phase_error(self) -> None:
        async with session_for_test() as session:
            with pytest.raises(PhaseError):
                await session.trigger_discovery()

    async def test_double_run_raises_phase_error(self) -> None:
        async with session_for_test() as session:
            await session.run()
            with pytest.raises(PhaseError):
                await session.run()

    async def test_start_on_a_started_session_is_a_no_op(self) -> None:
        session = session_for_test()
        actors = session.actors

        first = await session.start()
        second = await session.start()

        assert first is session
        assert second is session
        assert session.actors is actors, "start() must not rebuild the actors"

    async def test_shutdown_before_start_does_not_raise(self) -> None:
        session = session_for_test()
        await session.shutdown()

    async def test_closed_session_cannot_run(self) -> None:
        session = session_for_test()
        async with session:
            await session.run()
        # __aexit__ -> close() -> shutdown() -> Closed: run() is now a typed error.
        with pytest.raises(PhaseError):
            await session.run()

    async def test_closed_session_cannot_enqueue(self) -> None:
        session = session_for_test()
        async with session:
            pass
        with pytest.raises(PhaseError):
            await session.enqueue_path([], directions=None)
