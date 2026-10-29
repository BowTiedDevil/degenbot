"""Tests for the dual-await consumer.

`consume_result_batches` must derive its block clock from the forwarded
`newHeads` block stream (`engine.block_stream()`), NOT from
`ResultBatch["solve_block"]`. `solve_block` lags by the send debounce and only
advances when a batch is actually sent, so the old single-stream consumer froze
`[block: N]` behind the pump's `current_block`.

These tests inject fake block/result streams + a fake dispatch state (no live
RPC) and assert:
  - the block clock (`dispatcher.current_block`) tracks the BLOCK stream;
  - a result batch whose `solve_block` is stale does NOT move the clock;
  - dispatch is keyed off the block-stream current block.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from degenbot.dispatch import Dispatcher
from degenbot.runner._consume import consume_result_batches
from tests.fakes.runner_pipelines import StubPipeline
from tests.helpers.session import FakeAsyncW3, offline_cfg, session_for_test


class _Blocks:
    """Async iterator over a fixed list of block dicts, then StopAsyncIteration."""

    def __init__(self, blocks: list[dict[str, int]]) -> None:
        self._blocks = list(blocks)
        self._i = 0

    def __aiter__(self) -> _Blocks:
        return self

    async def __anext__(self) -> dict[str, int]:
        if self._i >= len(self._blocks):
            raise StopAsyncIteration
        b = self._blocks[self._i]
        self._i += 1
        return b


class _Results:
    """Async iterator over result-batch dicts, then StopAsyncIteration."""

    def __init__(self, batches: list[dict[str, Any]]) -> None:
        self._batches = list(batches)
        self._i = 0

    def __aiter__(self) -> _Results:
        return self

    async def __anext__(self) -> dict[str, Any]:
        if self._i >= len(self._batches):
            raise StopAsyncIteration
        b = self._batches[self._i]
        self._i += 1
        return b


def _block(number: int) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _empty_batch(solve_block: int) -> dict[str, Any]:
    """A result batch with empty fresh/updated/removed — no dispatch triggered."""
    return {
        "solve_block": solve_block,
        "timestamp": 1,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 0,
        "gas_limit": 30_000_000,
        "fresh": [],
        "updated": [],
        "expired": [],
        "removed": [],
    }


async def _run(
    blocks: list[dict[str, int]],
    batches: list[dict[str, Any]],
    *,
    dispatcher: Dispatcher | None = None,
    allow_quiet_end: bool = True,
) -> tuple[Dispatcher, FakeAsyncW3, list[int]]:
    dispatcher = dispatcher or Dispatcher.for_block(0)
    w3 = FakeAsyncW3()

    # SIMPIPE option A: the consumer routes batches through the pipeline; the
    # session's pipeline factory supplies the shared void stub, which records
    # the dispatch clock at enqueue. That clock is what `dispatched` proves —
    # the stub stands in for the serial leaf's own dispatch clock capture.
    StubPipeline.instances.clear()
    session = session_for_test(
        cfg=offline_cfg(live=False),
        dispatcher=dispatcher,
        current_block=dispatcher.current_block,
        async_w3=w3,
    )
    await session.build_pipeline()
    await consume_result_batches(
        session.block_loop(),
        block_stream=_Blocks(blocks),
        result_iter=_Results(batches),
        allow_quiet_end=allow_quiet_end,
    )
    dispatched: list[int] = []
    for stub in StubPipeline.instances:
        dispatched.extend(stub.enqueue_clock)
    return dispatcher, w3, dispatched


async def _run_with_pipeline_factory(
    factory: Any,
    *,
    blocks: list[dict[str, int]],
    batches: list[dict[str, Any]],
    allow_quiet_end: bool = True,
) -> tuple[Dispatcher, FakeAsyncW3, list[int]]:
    """Drive the consumer with an injected pipeline factory (drain tests)."""
    dispatcher = Dispatcher.for_block(0)
    w3 = FakeAsyncW3()

    StubPipeline.instances.clear()
    session = session_for_test(
        cfg=offline_cfg(live=False),
        dispatcher=dispatcher,
        current_block=dispatcher.current_block,
        async_w3=w3,
        pipeline_factory=factory,
    )
    await session.build_pipeline()
    await consume_result_batches(
        session.block_loop(),
        block_stream=_Blocks(blocks),
        result_iter=_Results(batches),
        allow_quiet_end=allow_quiet_end,
    )
    dispatched: list[int] = []
    for stub in StubPipeline.instances:
        dispatched.extend(getattr(stub, "enqueue_clock", []))
    return dispatcher, w3, dispatched


class TestBlockClockFromStream:
    async def test_block_clock_tracks_block_stream_not_solve_block(self) -> None:
        # Block stream advances 101 -> 102 -> 103. Result batches carry a stale
        # solve_block=999 to prove the clock ignores it.
        dispatcher, _w3, _dispatched = await _run(
            blocks=[_block(101), _block(102), _block(103)],
            batches=[_empty_batch(999), _empty_batch(999)],
        )
        assert dispatcher.current_block == 103, (
            "block clock must track the block stream, not solve_block"
        )

    async def test_block_ticks_drive_the_time_ring(self) -> None:
        # The per-block advance drives the clock + records block-time pairs (the
        # latency-baseline ring the fee-history leaf feeds). The fee-history RPC
        # itself runs in the Rust submit leaf, covered by its own tests.
        dispatcher, _w3, _dispatched = await _run(
            blocks=[_block(201), _block(202)],
            batches=[],
        )
        assert dispatcher.current_block == 202, (
            "block clock must track the block stream through the fee-history leaf"
        )
        assert dispatcher.block_time_count() >= 2, (
            "block-time ring must record one pair per block-stream tick "
            "(the latency baseline + the fee-history clock source)"
        )

    async def test_dispatch_keys_off_block_stream_current_block(self) -> None:
        # A non-empty batch arrives with a deliberately-stale solve_block=999.
        # dispatch must use the dispatcher's CURRENT block (the block-stream
        # clock), NEVER the batch's solve_block — regardless of whether the
        # block stream has ticked past the batch's block yet (racing is
        # expected under asyncio.wait; the load-bearing claim is that the
        # batch's solve_block is never used as the clock).
        batch = dict(_empty_batch(999))
        batch["fresh"] = [
            (1, 100, 50, (1, 2), (3,), (0,))
        ]  # one profitable result (state_nonces=(0,))
        _dispatcher, _w3, dispatched = await _run(
            blocks=[_block(301), _block(302)],
            batches=[batch],
        )
        assert len(dispatched) == 1
        # The load-bearing contract: dispatch must NEVER use the batch's
        # solve_block (999) as the current block — only the block-stream clock
        # (the seed 0 or a ticked 301/302, depending on race order).
        assert dispatched[0] != 999, (
            "dispatch must key off the block-stream clock, never the batch's stale solve_block"
        )


class TestPumpDeathVisible:
    """Incident 2026-08-20: a dead pump's ended delivery channels must abort
    the consumer loudly, not return quietly (the quiet end looked like a
    GIL deadlock to operators - the process sat with a live-but-dead
    settlement bot)."""

    async def test_block_stream_end_aborts_loudly(self) -> None:
        # Both streams end naturally (the Rust side drops both delivery
        # channels together when the pump's WS subscription dies).
        with pytest.raises(RuntimeError, match="stream ended"):
            await _run(
                blocks=[_block(101)],
                batches=[_empty_batch(101)],
                allow_quiet_end=False,
            )

    async def test_quiet_end_suppressed_for_injected_streams(self) -> None:
        # The harness flag keeps finite injected streams quiet (existing
        # dual-clock tests are not about stream death).
        dispatcher, _w3, _dispatched = await _run(
            blocks=[_block(101)],
            batches=[],
            allow_quiet_end=True,
        )
        assert dispatcher.current_block == 101


class _AbsorbingDrainPipeline:
    """A pipeline whose outcome stream absorbs the drain task's cancellation."""

    def __init__(self, session: object, **kwargs: object) -> None:
        self.session = session
        StubPipeline.instances.append(self)

    def enqueue(self, work: object) -> None:
        return None

    def raise_if_failed(self) -> None:
        return None

    async def next_outcome(self) -> None:
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            return  # the swallow the drain reap must catch

    async def shutdown(self) -> None:
        return None

    async def stop(self) -> None:
        return None


class _LoudAbortPipeline(_AbsorbingDrainPipeline):
    """A pipeline whose outcome stream dies with the loud-abort failure."""

    async def next_outcome(self) -> None:
        msg = "loud abort from the submit lane"

        raise RuntimeError(msg)


class TestDrainReapDiscipline:
    """The outcome-drain teardown at consumer stream end.

    The drain's lifetime is the consumer loop's: at stream end the loop
    cancels + reaps it, and the reap must OBSERVE the cancellation (a drain
    that absorbed its cancel cannot pass silently) while a drain that
    already died with the loud-abort failure still surfaces it.
    """

    async def test_stream_end_observes_drain_cancellation(self) -> None:
        with pytest.raises(AssertionError, match="cancellation"):
            await _run_with_pipeline_factory(_AbsorbingDrainPipeline, blocks=[], batches=[])

    async def test_ended_drain_still_surfaces_loud_abort(self) -> None:
        with pytest.raises(RuntimeError, match="loud abort"):
            await _run_with_pipeline_factory(_LoudAbortPipeline, blocks=[], batches=[])
