"""Block-stream + result-batch consumption for the settlement-arbitrage cockpit.

Extracted from ``examples/eth_backrun_v2_v3_v4_runner.py``.
Owns the permanent main loop: :func:`consume_result_batches` awaits
the block clock (``bot.block_stream()``) and the result batches (``engine``)
concurrently, driving the dispatcher's block clock and handing profitable
results to the batch pipeline.

The loop reads its collaborators off ONE :class:`BlockLoop` record (the
dispatcher, the async provider, the attached sim/submit pipeline, the engine
result stream, the fee-history leaf, the display policy, the smoke FSM, and the
session's block-clock mutator) — never a session bag, so every read is a named
field of the record the caller assembled.

The single-consumer pump block stream is consumed directly here (no tee): the
redundant Python recurring liquidity-map re-verify and its block-stream fan-out
were removed — pools are verified by the Rust two-step seed/drain gate and the
Rust solve-time solver-state verifier, not a Python whole-batch re-check.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast

from degenbot.calculations import next_base_fee
from degenbot.config import resolved_config
from degenbot.diagnostics import mark_progress
from degenbot.dispatch import typed_submit_record
from degenbot.logging import logger as bot_logger
from degenbot.runner._dispatch import (
    RawEngineResult,
    track_submission_smoke,
)
from degenbot.runner._render import DisplayPolicy, MergedOutcome, render_outcome
from degenbot.runner._sim_submit import BatchWork
from degenbot.utils.tasks import cancel_and_reap

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable, Sequence

    from degenbot.dispatch import Dispatcher
    from degenbot.provider import AsyncAlloyProvider
    from degenbot.runner._dispatch import SubmissionSmoke


@dataclass(frozen=True)
class BlockLoop:
    """The block loop's collaborators, assembled once by the session.

    Every field is read by the loop: the dispatch state (``dispatcher``), the
    dispatch-path provider (``provider``), the attached sim/submit pipeline
    (``pipeline``), the engine result stream (``engine``), the head-tick
    fee-history leaf (``fee_history``), the render policy (``policy``), the
    per-session silent-veto FSM (``smoke``), and the session's block-clock
    mutator (``advance_block``).
    """

    dispatcher: Dispatcher
    engine: Any
    provider: AsyncAlloyProvider
    pipeline: Any
    fee_history: Callable[..., Awaitable[bool]]
    policy: DisplayPolicy
    smoke: SubmissionSmoke
    advance_block: Callable[[int], None]


async def consume_result_batches(
    loop: BlockLoop,
    *,
    block_stream: AsyncIterator[dict[str, int]],
    result_iter: AsyncIterator[dict[str, object]] | None = None,
    allow_quiet_end: bool = False,
) -> None:
    """Consume the block stream (clock) + result batches (dispatch) in parallel.

    The block clock comes from the forwarded ``newHeads`` stream
    (``bot.block_stream()``), NOT from ``ResultBatch.solve_block``. The result
    batch's ``solve_block`` lagged by the send debounce + only advanced when a
    batch was actually sent, so the bot's ``[block: N]`` froze behind the pump's
    ``current_block``. The block stream ticks once per accepted
    ``WsEvent::BlockHeader`` - the authoritative clock.

    The block stream is always injected - the coordinator owns the once-only
    ``Bot.block_stream()`` handle (ADR-027) and passes it in. The result
    iterator is injectable for testing; production pulls it from the engine.

    Args:
        loop: The loop's collaborators (dispatch state, provider, pipeline,
            engine, fee-history leaf, display policy, smoke FSM, block clock).
        block_stream: The forwarded ``newHeads`` tick stream.
        result_iter: The engine's solver-result batch stream (``None`` = the
            record's engine).
        allow_quiet_end: Whether a natural stream end returns quietly (test
            harness flag) instead of aborting loudly.

    Raises:
        RuntimeError: If either delivery stream ends while the bot is live (the
            pump stopped: a settlement bot cannot trade from a dead clock).

    """
    bot_logger.info("[consumer] Starting - block stream + result batches from Rust pump")
    # SIMPIPE option A: K-way concurrent sims + single ordered submitter. The
    # session built and attached the pipeline before the loop started; the
    # outcome drain is one task perching on the executor's record stream,
    # folding + rendering each drained batch for display.
    pipeline = loop.pipeline
    outcome_drain: asyncio.Task[None] | None = asyncio.ensure_future(
        _drain_batch_outcomes(loop, pipeline)
    )

    if result_iter is None:
        result_iter = aiter(loop.engine)

    block_fut = cast(
        "asyncio.Task[dict[str, int]] | None", asyncio.ensure_future(anext(block_stream))
    )
    result_fut = cast(
        "asyncio.Task[dict[str, object]] | None", asyncio.ensure_future(anext(result_iter))
    )
    block_ended = False
    result_ended = False

    while block_fut is not None or result_fut is not None:
        pending = {f for f in (block_fut, result_fut) if f is not None}
        done, _ = await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)

        for fut in done:
            if fut is block_fut:
                block_fut, block_ended = _reprime(block_stream, fut, "block stream")
                await _apply_block_if_ready(fut, loop)
            elif fut is result_fut:
                result_fut, result_ended = _reprime(result_iter, fut, "result stream")
                _apply_result_if_ready(fut, loop)
        # SIMPIPE loud-abort: a failed sim/submit leaf surfaces here (the same
        # kernel the consumer loop already brings down the run with).
        pipeline.raise_if_failed()
        # Mark main-loop forward progress for the Rust stuck-watchdog
        # (start_gil_probe). A stale timestamp here means the loop is parked
        # mid-`_apply_result_if_ready` (the dispatch deadlock site).
        mark_progress()

    # Incident 2026-08-20 (silent pump death): the Rust pump's delivery channels
    # (block clock + result batches) are owned by the engine, which OUTLIVES the
    # pump task. When the pump's WS subscription dies the Rust side drops them
    # (pump is STOPPED - ERROR log), which ends BOTH of these streams. A
    # settlement bot whose block clock is dead must abort loudly - a quiet
    # return here let the process sit as if the run were normal: operators read
    # it as a "GIL deadlock". Cancellation (explicit stop) raises CancelledError
    # inside asyncio.wait above and never reaches this point, so this only fires
    # on a natural stream end.
    # The drain task's lifetime is the consumer loop's: stop it at stream end
    # (the records still in flight die with the loop, exactly as the
    # pre-cut-over pipeline's leaves did). The reap asserts the drain observed
    # its cancellation, so a drain that outlives its cancel() cannot pass
    # silently before the loud-abort raise below.
    if outcome_drain is not None:
        if not outcome_drain.done():
            await cancel_and_reap(outcome_drain)
        elif not outcome_drain.cancelled():
            # The drain ended on its own; surface a stored loud-abort failure
            # (the loud-abort rule) rather than reaping it away.
            outcome_drain.result()

    if (block_ended or result_ended) and not allow_quiet_end:
        bot_logger.error(
            "[consumer] block/result stream ended - the Rust pump stopped "
            "(WS subscription dead or pump task exited); aborting"
        )
        message = (
            "block/result stream ended: the Rust pump stopped (WS subscription "
            "dropped or pump task exited - see the Rust 'pump is STOPPED' ERROR "
            "log). The settlement bot cannot keep trading from a dead block "
            "clock; aborting loudly."
        )
        raise RuntimeError(message)


def _reprime(
    stream: AsyncIterator[Any],
    fut: asyncio.Task[Any],
    label: str,
) -> tuple[asyncio.Task[Any] | None, bool]:
    """Schedule the next pull.

    Args:
        stream: The stream to pull the next item from.
        fut: The finished pull task.
        label: The log label naming the stream.

    Returns:
        The next-pull task (``None`` once the stream ended), and whether the
        stream ended.

    """
    try:
        fut.result()
    except StopAsyncIteration:
        bot_logger.info("[consumer] %s ended", label)
        return None, True
    except BaseException:
        return None, False
    return asyncio.ensure_future(anext(stream)), False


async def _apply_block_if_ready(
    fut: asyncio.Task[dict[str, int]],
    loop: BlockLoop,
    *,
    reward_percentiles: Sequence[int] | None = None,
) -> None:
    """Drive the block clock from a forwarded ``newHeads`` tick if fut resolved.

    Args:
        fut: The finished block-stream pull.
        loop: The loop's collaborators.
        reward_percentiles: The resolved core pair the caller hands down;
            ``None`` (production) reads the pair off the resolved verdict, so
            the values can never be hardcoded, cached, or mirrored here.

    """
    if fut.cancelled() or fut.exception() is not None:
        return
    dispatcher = loop.dispatcher
    provider = loop.provider
    try:
        block = fut.result()
    except StopAsyncIteration:
        return

    block_number = int(block["number"])
    block_timestamp = int(block["timestamp"])
    base_fee = int(block.get("base_fee_per_gas") or 0)
    gas_used = int(block["gas_used"])
    gas_limit = int(block["gas_limit"])

    base_fee_next = next_base_fee(
        parent_base_fee=base_fee,
        parent_gas_used=gas_used,
        parent_gas_limit=gas_limit,
    )

    # ``eth_feeHistory`` + hex-decode + ``record_priority_fees`` now happen in
    # the Rust submit leaf (``fetch_fee_history``). No-op on failure.
    async_alloy = provider.as_async_alloy()
    if async_alloy is not None:
        if reward_percentiles is None:
            reward_percentiles = resolved_config().fee_percentiles
        await loop.fee_history(
            provider=async_alloy,
            dispatcher=dispatcher,
            block_count=1,
            last_block=block_number,
            reward_percentiles=[float(p) for p in reward_percentiles],
        )

    dispatcher.record_block_time(block_number, block_timestamp)
    if dispatcher.block_time_count() >= 2:
        oldest_bn, _oldest_ts = dispatcher.block_times_oldest()
        if block_number != oldest_bn:
            latency = time.time() - block_timestamp
            # 2026-08-22 audit: this is a per-block METRIC, not an event — it
            # flooded stdout every block. Debug on the console; the scrapeable
            # latency signal lives in Prometheus (degenbot.submit.latency et
            # al.).
            bot_logger.debug(
                f"[block: {block_number}]"
                f"[latency: {latency:.1f}s]"
                f"[base fee: {base_fee / 10**9:.5f}, {base_fee_next / 10**9:.5f} next]",
            )

    dispatcher.advance_block(block_number)
    # The block clock advances through the session's mutator (the runner-side
    # frozen mirror is gone — every reader consults the session).
    loop.advance_block(block_number)


async def _drain_batch_outcomes(loop: BlockLoop, executor: Any) -> None:
    """Drain the executor's Batch outcome records for display.

    One drained batch = one render pass: the records fold into the render view
    (:meth:`MergedOutcome.from_records`), the renderers emit their
    ``[sim]``/``[profit]``/``[sim-fail]``/``[fot]`` lines, and the raw
    submit-lane records render the ``[dispatch]`` lines + the silent-veto smoke
    FSM. A stored leaf failure re-raises HERE (the loud-abort rule — the drain
    dies loudly inside the consumer's task set with the executor's own stored
    failure).

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
        # The drain frame re-raises the loud-abort failure (the consumer loop
        # polls the same surface between batches).
        executor.raise_if_failed()
        # Mark main-loop-adjacent forward progress for the Rust stuck-watchdog.
        mark_progress()


def _engine_result(item: Any, solve_block: int) -> RawEngineResult:
    """Shape one solver-result row into the named assembly-seam record.

    Args:
        item: The raw solver-result row from the engine batch.
        solve_block: The batch's solve block.

    Returns:
        The typed engine-result row, amounts coerced from the wire values.

    """
    path_id, opt_input, profit, hop_outs, consumed_ins, state_nonces = item
    return RawEngineResult(
        path_id=int(path_id),
        optimal_input=int(opt_input),
        engine_profit=int(profit),
        hop_outputs=tuple(int(h) for h in hop_outs),
        consumed_inputs=tuple(int(c) for c in consumed_ins),
        solve_block=solve_block,
        state_nonces=tuple(int(n) for n in state_nonces),
    )


def _apply_result_if_ready(
    fut: asyncio.Task[dict[str, object]],
    loop: BlockLoop,
) -> None:
    """Dispatch profitable results from a solver result batch if fut resolved.

    The batch's sim work is backgrounded K-way concurrent on the Rust-owned
    pipeline; submission happens on its SINGLE ordered submit lane, FIFO in
    batch arrival order - the nonce-serialization contract.

    Args:
        fut: The finished result-batch pull.
        loop: The loop's collaborators (the pipeline is enqueued on).

    """
    if fut.cancelled() or fut.exception() is not None:
        return
    try:
        batch = fut.result()
    except StopAsyncIteration:
        return

    dispatcher = loop.dispatcher
    current_block = dispatcher.current_block
    solve_block = int(cast("Any", batch["solve_block"]))

    results = [_engine_result(item, solve_block) for item in cast("Any", batch["fresh"])]
    results += [_engine_result(item, solve_block) for item in cast("Any", batch["updated"])]

    for path_id in cast("Any", batch["removed"]):
        dispatcher.discard_path(int(path_id))

    # SIMPIPE2 T3: the engine's inline-sim payloads (empty dict = the legacy
    # FFI-sim path for every entry — per-entry presence decides).
    payloads: dict[int, dict[str, Any]] = {
        int(pid): payload for pid, payload in cast("Any", batch.get("payloads") or {}).items()
    }

    if results or payloads:
        loop.pipeline.enqueue(
            BatchWork(
                results=results,
                block_timestamp=dispatcher.block_timestamp_for(current_block) or 0,
                base_fee_next=next_base_fee(
                    parent_base_fee=int(cast("Any", batch.get("base_fee_per_gas") or 0)),
                    parent_gas_used=int(cast("Any", batch["gas_used"])),
                    parent_gas_limit=int(cast("Any", batch["gas_limit"])),
                ),
                current_block=current_block,
                payloads=payloads,
            )
        )
