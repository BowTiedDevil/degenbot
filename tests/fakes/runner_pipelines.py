"""Fakes for the runner's sim-submit pipeline wiring.

``StubPipeline`` voids the pipeline at the session factory seam (block-loop
tests drive streams, not sim leaves). It records the session owner it received
(the "one owner" contract) and the block clock carried by each enqueued batch;
:attr:`StubPipeline.instances` collects the instances one drive created (clear
it before driving to isolate a test's captures).

The real pipeline's mechanism (bounded concurrency, FIFO submit order, loud
abort) is Rust-owned and tested in ``degenbot-submission``; the Python side is
covered by the knob/adapter tests in ``tests/arbitrage/test_sim_submit_pipeline``.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, ClassVar

if TYPE_CHECKING:
    from degenbot.runner._sim_submit import BatchWork


class StubPipeline:
    """Void stand-in for the FFI pipeline at the session factory seam."""

    instances: ClassVar[list[StubPipeline]] = []

    def __init__(self, session: object, **kwargs: object) -> None:
        self.session = session
        self.enqueue_clock: list[int] = []
        StubPipeline.instances.append(self)

    def enqueue(self, work: BatchWork) -> None:
        # The real pipeline stamps the batch's block at enqueue time; the stub
        # records the value the consumer carried so the clock contract is
        # observable without the Rust sim seam.
        self.enqueue_clock.append(work.current_block)

    def raise_if_failed(self) -> None:
        return None

    async def shutdown(self) -> None:
        return None

    async def stop(self) -> None:
        return None
