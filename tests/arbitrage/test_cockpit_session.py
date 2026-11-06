"""Cockpit session state — the one owner of pump-session coordination state.

The block loop and the dispatch path must read the SAME session the cockpit
owns, instead of the session travelling as a 10-parameter signature + 9
kwargs. The drain adapter receives its collaborators as one
:class:`~degenbot.runner._consume.BlockLoop` record assembled from that
session, and the sim/submit pipeline (which the session's core consumer
enqueues on) is built over the same owner.

The block clock is not Python's: the Rust session consumer writes the session's
dispatcher, and ``Session.current_block`` reads it. A test that needs the clock
to move replays the engine's stream shape through the shipped consumer
(``tests.helpers.session_drive``).

Seams: the public session lifecycle (an injected consumer factory observes the
owner) and the drain adapter (``consume_result_batches`` driven with the
session's collaborator record + a scripted core consumer). No anvil, no live
RPC.
"""

from __future__ import annotations

import signal

import pytest

from tests.fakes.engine import FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline
from tests.helpers.boot_actors import quiet_report_coro
from tests.helpers.session import session_for_test
from tests.helpers.session_drive import block_event, drive_session, session_with_real_clock


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


class TestSessionOwner:
    async def test_runner_consumer_receives_the_owner(self) -> None:
        """The consumer task and its pipeline are built over the SAME owner."""
        captured: dict[str, object] = {}

        def capturing_consumer(loop: object, consumer: object) -> object:
            captured["loop"] = loop
            captured["consumer"] = consumer
            return quiet_report_coro()

        StubPipeline.instances.clear()
        session = session_for_test(consumer=capturing_consumer)
        async with session:
            await session.run()

        loop = captured["loop"]
        assert loop is not None, "the consumer must be handed the loop's collaborators"
        assert loop.pipeline is session.sim_submit_pipeline  # type: ignore[attr-defined]
        # SIMPIPE: the "same owner" contract rides the pipeline seam — the
        # session built the pipeline over itself, and the consumer enqueues on it.
        assert [stub.session for stub in StubPipeline.instances] == [session]

    async def test_the_drain_reads_the_session_pipeline(self) -> None:
        StubPipeline.instances.clear()
        session = session_for_test()
        await session.build_pipeline()

        loop = session.block_loop()

        assert loop.pipeline is session.sim_submit_pipeline
        assert [stub.session for stub in StubPipeline.instances] == [session], (
            "the dispatch pipeline did not receive the session owner"
        )

    async def test_the_consumer_advances_the_session_block_clock(self) -> None:
        """The session's block clock is the dispatcher the consumer writes."""
        session, executor = await session_with_real_clock()
        start_block = session.current_block

        consumer = await drive_session(session, [block_event(start_block + 2)])

        assert session.current_block == start_block + 2
        assert consumer.beats == 1, "one received stream event, one beat"
        assert executor.enqueued == 0, "a block carries no batch work"

    async def test_the_consumer_is_built_over_the_session_and_its_pipeline(self) -> None:
        """The consumer is built from the session's own registry-backed actors."""
        registry = FakeEngineRegistry()
        session = session_for_test(engine_registry=registry)

        await session.run()

        assert session.actors is not None
        assert session.actors.engine_registry is registry
        consumer = session.consumer
        assert consumer is not None
        assert consumer.session is session
        assert consumer.executor is session.sim_submit_pipeline
        assert len(registry.start_calls) == 0, (
            "a for_test session resolves without driving the engine's start ritual"
        )
