"""Fakes for the runner's pipeline and consumer seams.

**The sim-submit pipeline factory seam.** ``StubPipeline`` voids the pipeline at
the session factory seam (runner tests drive the drain, not sim leaves). It
records the session owner it received (the "one owner" contract);
:attr:`StubPipeline.instances` collects the instances one drive created (clear
it before driving to isolate a test's captures).

**The session-consumer factory seam.** ``FakeSessionConsumer`` stands in for the
core consumer (`degenbot.dispatch.SessionConsumer`): it records the session's
start flow (attach → resume → run) so a cockpit test can assert the ordering
without a live pump, and it answers the same typed report the core's loop
returns.

The real pipeline's mechanism (bounded concurrency, FIFO submit order, loud
abort) and the real consume loop are Rust-owned and tested in
``degenbot-submission`` / ``degenbot-batch-executor``; the Python side is covered
by the knob/adapter tests in ``tests/arbitrage/test_sim_submit_pipeline``.
"""

from __future__ import annotations

from typing import Any, ClassVar


class StubPipeline:
    """Void stand-in for the FFI pipeline at the session factory seam."""

    instances: ClassVar[list[StubPipeline]] = []

    def __init__(self, session: object, **kwargs: object) -> None:
        self.session = session
        StubPipeline.instances.append(self)

    async def next_outcome(self) -> None:
        # The executor's drain loop terminator: the void stub publishes no
        # records, so the drain ends immediately.
        return None


class FakeSessionConsumer:
    """Void stand-in for the core session consumer at the factory seam.

    The consumer's own loop is Rust-owned; this double answers the session's
    calls in order (``attach`` → ``resume`` → ``run``) and reports a quiet end,
    so a cockpit test observes the start flow and the drain seam without a live
    pump.
    """

    instances: ClassVar[list[FakeSessionConsumer]] = []

    def __init__(self, session: object, executor: object = None) -> None:
        self.session = session
        self.executor = executor
        self.calls: list[str] = []
        self.attached_to: object | None = None
        self.end = "quiet-end"
        FakeSessionConsumer.instances.append(self)

    def attach(self, engine: object) -> None:
        """Record the engine whose hand-off the consumer took."""
        self.attached_to = engine
        self.calls.append("attach")

    async def resume(self) -> None:
        """Arm the pump (the real consumer consumes the session's claim)."""
        self.calls.append("resume")

    async def run(self) -> dict[str, Any]:
        """Report a finished loop without consuming anything."""
        self.calls.append("run")
        return {
            "batches": 0,
            "blocks": 0,
            "stale_batches": 0,
            "end_of_stream": 1,
            "end": self.end,
        }


def fake_session_consumer(session: object, executor: object = None) -> FakeSessionConsumer:
    """The injection seam's default consumer factory."""
    return FakeSessionConsumer(session, executor)
