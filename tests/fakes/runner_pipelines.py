"""Fakes for the runner's pipeline seams.

**The ``PathRegistrationPipeline`` construction seam.** The constructor reads a
context slice (``bot``, ``chain_id``, ``database_path``, the tracker slots,
``weth``) and, off the bot, the intake gate plus the Rust meter handle.
:class:`FakePipelineContext` + :class:`FakeFleetHostedBot` build exactly that
slice; the pipeline reads both bot fields with ``getattr`` defaults, so an
unset ``_py_bot`` and a missing one behave identically.

**The sim-submit pipeline factory seam.** ``StubPipeline`` voids the pipeline
at the session factory seam (block-loop tests drive streams, not sim leaves).
It records the session owner it received
(the "one owner" contract) and the block clock carried by each enqueued batch;
:attr:`StubPipeline.instances` collects the instances one drive created (clear
it before driving to isolate a test's captures).

The real pipeline's mechanism (bounded concurrency, FIFO submit order, loud
abort) is Rust-owned and tested in ``degenbot-submission``; the Python side is
covered by the knob/adapter tests in ``tests/arbitrage/test_sim_submit_pipeline``.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
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


@dataclass
class FakeFleetHostedBot:
    """Bot double for pipeline construction: the intake gate + meter handle.

    ``registration_fleet_hosted`` is the gate the pipeline reads at
    construction; ``_py_bot`` is the Rust meter handle the skip recorder
    forwards to (``None`` = count only). Both are read with ``getattr``
    defaults, so a field the test leaves unset stays unset.
    """

    fleet_hosted: bool = True
    _py_bot: object | None = None

    def registration_fleet_hosted(self) -> bool:
        return self.fleet_hosted


@dataclass
class FakePipelineContext:
    """The construction-context slice ``PathRegistrationPipeline`` reads."""

    bot: object
    chain_id: int = 1
    database_path: Path = Path("unused.db")
    uniswap_v3_tracker: object | None = None
    sushiswap_v3_tracker: object | None = None
    pancakeswap_v3_tracker: object | None = None
    weth: object | None = None
