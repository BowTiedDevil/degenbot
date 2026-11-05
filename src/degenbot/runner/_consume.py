"""The session consumer's adapter: the record drain and the report.

The consume loop itself — the block clock, the batch-work derivation, the
ordered lane enqueue, the loud-abort rule, the stall feed, and the session
close — is the Rust core's ONE implementation
(``degenbot_batch_executor::session``), driven through
:class:`degenbot.dispatch.SessionConsumer`. This module contributes only what an
adapter owns in Python:

* the display drain of the executor's Batch outcome records (the render fold and
  the ``[dispatch]`` lines), which the loop quiesces when it ends;
* the report's rendering: a ``pump-stopped`` end verdict is the loud abort a
  supervised settlement session must not survive, and its message is the
  cockpit's operator-facing vocabulary.

Nothing here decides a candidate's fate: a renderer reads the typed records and
emits lines (``stays-python``); every sim/submit arithmetic runs in the core.
"""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast

from degenbot.diagnostics import mark_progress
from degenbot.dispatch import typed_submit_record
from degenbot.logging import logger as bot_logger
from degenbot.runner._dispatch import track_submission_smoke
from degenbot.runner._render import MergedOutcome, render_outcome

if TYPE_CHECKING:
    from degenbot.dispatch import Dispatcher
    from degenbot.runner._dispatch import SubmissionSmoke
    from degenbot.runner._render import DisplayPolicy


#: Incident 2026-08-20 (silent pump death): the pump's delivery stream is owned
#: by the engine, which OUTLIVES the pump task. When the pump's WS subscription
#: dies the Rust side closes the stream, so a supervised session sees a natural
#: end. A settlement bot whose block clock is dead must abort loudly — a quiet
#: return let the process sit as if the run were normal, which operators read as
#: a "GIL deadlock". Cancellation (explicit stop) cancels the consumer task and
#: never reaches this raise, so this only fires on a natural stream end.
_PUMP_STOPPED_MESSAGE = (
    "block/result stream ended: the Rust pump stopped (WS subscription "
    "dropped or pump task exited - see the Rust 'pump is STOPPED' ERROR "
    "log). The settlement bot cannot keep trading from a dead block "
    "clock; aborting loudly."
)


@dataclass(frozen=True)
class BlockLoop:
    """The display drain's collaborators, assembled once by the session.

    Every field is read by the drain: the dispatch state (``dispatcher``), the
    attached sim/submit pipeline (``pipeline``), the render policy (``policy``),
    and the per-session silent-veto FSM (``smoke``). The block clock, the batch
    work, and the loud-abort rule are NOT here — the core consumer owns them.
    """

    dispatcher: Dispatcher
    pipeline: Any
    policy: DisplayPolicy
    smoke: SubmissionSmoke


async def consume_result_batches(loop: BlockLoop, consumer: Any) -> None:
    """Run the session consumer, draining its record stream for display.

    The consumer has already attached the engine's hand-off and started
    production (its ``resume`` is the claim's one use), so this adapter's share is
    the drain plus the report: the loop publishes the executor's typed records,
    the drain renders each drained batch, and a natural end under the supervised
    policy is re-raised here in the caller's frame.

    Args:
        loop: The drain's collaborators (dispatch state, pipeline, render
            policy, smoke FSM).
        consumer: The session consumer handle (``resume``/``run``).

    Raises:
        RuntimeError: If the ordered stream ended outside this session's own
            teardown (``pump-stopped``: a settlement bot cannot trade from a
            dead clock). A leaf failure surfaces as the consumer's own
            ``SessionConsumerError``, before this adapter sees a report.

    """
    bot_logger.info("[consumer] Starting - the session consumer owns the loop")

    drain = asyncio.ensure_future(_drain_batch_outcomes(loop, loop.pipeline))
    try:
        report = await consumer.run()
    finally:
        # The loop quiesces the lane when it ends, so the drain terminates on
        # its own; a cancelled consumer (explicit teardown) leaves it parked,
        # which is the one case that needs the cancel.
        if not drain.done():
            drain.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await drain

    if cast("dict[str, Any]", report)["end"] == "pump-stopped":
        bot_logger.error(
            "[consumer] block/result stream ended - the Rust pump stopped "
            "(WS subscription dead or pump task exited); aborting"
        )
        raise RuntimeError(_PUMP_STOPPED_MESSAGE)


async def _drain_batch_outcomes(loop: BlockLoop, executor: Any) -> None:
    """Drain the executor's Batch outcome records for display.

    One drained batch = one render pass: the records fold into the render view
    (:meth:`MergedOutcome.from_records`), the renderers emit their
    ``[sim]``/``[profit]``/``[sim-fail]``/``[fot]`` lines, and the raw
    submit-lane records render the ``[dispatch]`` lines + the silent-veto smoke
    FSM.

    Args:
        loop: The loop's collaborators (the display policy + smoke FSM live
            here).
        executor: The batch executor whose outcome stream is drained.

    """
    while True:
        batch = await executor.next_outcome()
        if batch is None:
            return
        outcome = MergedOutcome.from_records(batch.records)
        if outcome:
            block = batch.records[0].block if batch.records else loop.dispatcher.current_block
            render_outcome(
                outcome,
                dispatcher=loop.dispatcher,
                current_block=block,
                policy=loop.policy,
            )
            records = [typed_submit_record(record) for record in batch.submit_records]
            track_submission_smoke(loop.smoke, outcome, records, policy=loop.policy)
        # Mark main-loop-adjacent forward progress for the Rust stuck-watchdog.
        mark_progress()
