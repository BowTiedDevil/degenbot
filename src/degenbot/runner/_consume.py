"""Block-stream + result-batch consumption for the settlement-arbitrage ``BotRunner``.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` (epic 5TSYKN, task
CXWQDI). Owns the permanent main loop: :func:`consume_result_batches` awaits
the block clock (``engine.block_stream()``) and the result batches
(``engine``) concurrently, driving the dispatcher's block clock and dispatching
profitable results through the Rust seam.

The single-consumer pump block stream is consumed directly here (no tee): the
redundant Python recurring liquidity-map re-verify and its block-stream fan-out
were removed — pools are verified by the Rust two-step seed/drain gate and the
Rust solve-time solver-state verifier, not a Python whole-batch re-check.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import AsyncIterator
from typing import TYPE_CHECKING, Any, cast

from degenbot.calculations import next_base_fee
from degenbot.diagnostics import mark_progress
from degenbot.dispatch import fetch_fee_history
from degenbot.logging import logger as bot_logger
from degenbot.runner._dispatch import _dispatch_profitable
from degenbot.runner._driver_constants import FEE_PERCENTILES

if TYPE_CHECKING:
    from degenbot.runner.bot_runner import _SessionState


async def consume_result_batches(
    session: _SessionState,
    *,
    block_stream: AsyncIterator[dict[str, int]] | None = None,
    result_iter: AsyncIterator[dict[str, object]] | None = None,
) -> None:
    """Consume the block stream (clock) + result batches (dispatch) in parallel.

    Epic 6W35AI: the block clock comes from the forwarded ``newHeads`` stream
    (``engine.block_stream()``), NOT from ``ResultBatch.solve_block``. The
    result batch's ``solve_block`` lagged by the send debounce + only advanced
    when a batch was actually sent, so the bot's ``[block: N]`` froze behind
    the pump's ``current_block``. The block stream ticks once per accepted
    ``WsEvent::BlockHeader`` — the authoritative clock.

    Both streams are injectable for testing; production pulls them from the
    engine.
    """
    bot_logger.info("[consumer] Starting — block stream + result batches from Rust pump")

    if block_stream is None:
        block_stream = session.engine_registry.engine.block_stream()
    if result_iter is None:
        result_iter = aiter(session.engine_registry.engine)

    block_fut = cast(
        "asyncio.Task[dict[str, int]] | None", asyncio.ensure_future(anext(block_stream))
    )
    result_fut = cast(
        "asyncio.Task[dict[str, object]] | None", asyncio.ensure_future(anext(result_iter))
    )

    while block_fut is not None or result_fut is not None:
        pending = {f for f in (block_fut, result_fut) if f is not None}
        done, _ = await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)

        for fut in done:
            if fut is block_fut:
                block_fut = cast(
                    "asyncio.Task[dict[str, int]] | None",
                    _reprime(block_stream, fut, "block stream"),
                )
                await _apply_block_if_ready(fut, session)
            elif fut is result_fut:
                result_fut = cast(
                    "asyncio.Task[dict[str, object]] | None",
                    _reprime(result_iter, fut, "result stream"),
                )
                await _apply_result_if_ready(fut, session)
        # ergo 66H3KJ: mark main-loop forward progress for the Rust stuck-
        # watchdog (start_gil_probe). A stale timestamp here means the loop
        # is parked mid-`_apply_result_if_ready` (the dispatch deadlock site).
        mark_progress()


def _reprime(
    stream: AsyncIterator[Any],
    fut: asyncio.Task[Any],
    label: str,
) -> asyncio.Task[Any] | None:
    """If `fut`'s stream ended, return None; else schedule the next pull."""
    try:
        fut.result()
    except StopAsyncIteration:
        bot_logger.info("[consumer] %s ended", label)
        return None
    except BaseException:
        return None
    return asyncio.ensure_future(anext(stream))


async def _apply_block_if_ready(fut: asyncio.Task[dict[str, int]], session: _SessionState) -> None:
    """Drive the block clock from a forwarded ``newHeads`` tick if fut resolved."""
    if fut.cancelled() or fut.exception() is not None:
        return
    dispatcher = session.dispatcher
    async_w3 = session.async_w3
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

    # 7UIYJ6: ``eth_feeHistory`` + hex-decode + ``record_priority_fees`` now
    # happen in the Rust submit leaf (``fetch_fee_history``). No-op on failure.
    async_alloy = async_w3.as_async_alloy()
    if async_alloy is not None:
        await fetch_fee_history(
            provider=async_alloy,
            dispatcher=dispatcher,
            block_count=1,
            last_block=block_number,
            reward_percentiles=[float(p) for p in FEE_PERCENTILES],
        )

    dispatcher.record_block_time(block_number, block_timestamp)
    if dispatcher.block_time_count() >= 2:
        oldest_bn, _oldest_ts = dispatcher.block_times_oldest()
        if block_number != oldest_bn:
            latency = time.time() - block_timestamp
            bot_logger.info(
                f"[block: {block_number}]"
                f"[latency: {latency:.1f}s]"
                f"[base fee: {base_fee / 10**9:.5f}, {base_fee_next / 10**9:.5f} next]",
            )

    dispatcher.advance_block(block_number)
    session.current_block = block_number


async def _apply_result_if_ready(
    fut: asyncio.Task[dict[str, object]], session: _SessionState
) -> None:
    """Dispatch profitable results from a solver result batch if fut resolved."""
    if fut.cancelled() or fut.exception() is not None:
        return
    try:
        batch = fut.result()
    except StopAsyncIteration:
        return

    current_block = session.dispatcher.current_block
    operator_nonce = await session.async_w3.get_transaction_count(session.cfg.operator_address)
    solve_block = int(cast("Any", batch["solve_block"]))

    results: list[tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]] = []
    for item in cast("Any", batch["fresh"]):
        path_id, opt_input, profit, hop_outs, consumed_ins, state_nonces = item
        results.append((
            int(path_id),
            int(opt_input),
            int(profit),
            tuple(int(h) for h in hop_outs),
            tuple(int(c) for c in consumed_ins),
            solve_block,
            tuple(int(n) for n in state_nonces),
        ))
    for item in cast("Any", batch["updated"]):
        path_id, opt_input, profit, hop_outs, consumed_ins, state_nonces = item
        results.append((
            int(path_id),
            int(opt_input),
            int(profit),
            tuple(int(h) for h in hop_outs),
            tuple(int(c) for c in consumed_ins),
            solve_block,
            tuple(int(n) for n in state_nonces),
        ))

    for path_id in cast("Any", batch["removed"]):
        session.dispatcher.discard_path(int(path_id))

    if results:
        await _dispatch_profitable(
            session,
            results,
            block_timestamp=session.dispatcher.block_timestamp_for(current_block) or 0,
            base_fee_next=next_base_fee(
                parent_base_fee=int(cast("Any", batch.get("base_fee_per_gas") or 0)),
                parent_gas_used=int(cast("Any", batch["gas_used"])),
                parent_gas_limit=int(cast("Any", batch["gas_limit"])),
            ),
            operator_nonce=operator_nonce,
        )
