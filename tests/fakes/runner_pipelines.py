"""Fakes for the runner's pipeline and consumer seams.

**The sim-submit pipeline factory seam.** ``StubPipeline`` voids the pipeline at
the session factory seam (runner tests drive the drain, not sim leaves). It
records the session owner it received (the "one owner" contract);
:func:`stub_pipeline_capture` hands one test both the factory to inject and the
capture of the stubs that factory built.

**The session-consumer factory seam.** ``FakeSessionConsumer`` stands in for the
core consumer (`degenbot.dispatch.SessionConsumer`): it records the session's
start flow (attach → resume → run) so a cockpit test can assert the ordering
without a live pump, and it answers the same typed report the core's loop
returns. The constructed consumer is that test's capture of the seam — the
session, the executor, and the call order — read back through the session.

The real pipeline's mechanism (bounded concurrency, FIFO submit order, loud
abort) and the real consume loop are Rust-owned and tested in
``degenbot-submission`` / ``degenbot-batch-executor``; the Python side is covered
by the knob/adapter tests in ``tests/arbitrage/test_sim_submit_pipeline``.
"""

from __future__ import annotations

from typing import Any


class StubPipeline:
    """Void stand-in for the FFI pipeline at the session factory seam."""

    def __init__(self, session: object, **kwargs: object) -> None:
        self.session = session

    async def next_outcome(self) -> None:
        # The executor's drain loop terminator: the void stub publishes no
        # records, so the drain ends immediately.
        return None


class StubPipelineCapture:
    """One drive's capture of the stub pipelines its injected factory built.

    ``factory`` is the pipeline-factory seam to inject; ``instances`` holds the
    stubs it created. Each capture sees only its own drive, so a test's
    assertions hold whatever the other tests did and in whatever order the
    suite runs them.
    """

    def __init__(self) -> None:
        self.instances: list[StubPipeline] = []

    def factory(self, session: object, **kwargs: object) -> StubPipeline:
        stub = StubPipeline(session, **kwargs)
        self.instances.append(stub)
        return stub


def stub_pipeline_capture() -> StubPipelineCapture:
    """A fresh per-test pipeline capture: its factory plus its recorded stubs."""
    return StubPipelineCapture()


class FakeSessionConsumer:
    """Void stand-in for the core session consumer at the factory seam.

    The consumer's own loop is Rust-owned; this double answers the session's
    calls in order (``attach`` → ``resume`` → ``run``) and reports a quiet end,
    so a cockpit test observes the start flow and the drain seam without a live
    pump.
    """

    def __init__(self, session: object, executor: object = None) -> None:
        self.session = session
        self.executor = executor
        self.calls: list[str] = []
        self.attached_to: object | None = None
        self.end = "quiet-end"

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
