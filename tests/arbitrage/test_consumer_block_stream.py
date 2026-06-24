"""Red→Green tests for the dual-await consumer (epic 6W35AI).

`consume_result_batches` must derive its block clock from the forwarded
`newHeads` block stream (`engine.block_stream()`), NOT from
`ResultBatch["solve_block"]`. `solve_block` lags by the send debounce and only
advances when a batch is actually sent, so the old single-stream consumer froze
`[block: N]` behind the pump's `current_block`.

These tests inject fake block/result streams + a fake `async_w3` (no live RPC)
and assert:
  - the block clock (`dispatcher.current_block`) tracks the BLOCK stream;
  - a result batch whose `solve_block` is stale does NOT move the clock;
  - dispatch is keyed off the block-stream current block.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

import examples.eth_backrun_v2_v3_v4_rust as runner
from examples.eth_backrun_v2_v3_v4_rust import Dispatcher


class _Eth:
    """Fake eth namespace — records fee_history blocks + nonce."""

    def __init__(self, nonce: int = 5) -> None:
        self._nonce = nonce
        self.fee_history_blocks: list[int] = []

    async def fee_history(self, *, block_count: int, newest_block: int, reward_percentiles):
        self.fee_history_blocks.append(int(newest_block))
        return {"reward": [[]]}  # empty reward → no priority-fee recording

    async def get_transaction_count(self, address: str) -> int:  # noqa: ARG002
        return self._nonce


class _FakeW3:
    def __init__(self) -> None:
        self.eth = _Eth()


class _Blocks:
    """Async iterator over a fixed list of block dicts, then StopAsyncIteration."""

    def __init__(self, blocks: list[dict[str, int]]) -> None:
        self._blocks = list(blocks)
        self._i = 0

    def __aiter__(self) -> "_Blocks":
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

    def __aiter__(self) -> "_Results":
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
) -> tuple[Dispatcher, _FakeW3]:
    dispatcher = dispatcher or Dispatcher.for_block(0)
    w3 = _FakeW3()
    # Monkeypatch dispatch_profitable_results so a non-empty batch records the
    # `current_block` it was dispatched with, proving it keys off the block
    # stream (not solve_block).
    dispatched: list[int] = []

    async def _fake_dispatch(**kwargs):
        dispatched.append(kwargs["current_block"])
        return None

    orig = runner.dispatch_profitable_results
    runner.dispatch_profitable_results = _fake_dispatch  # type: ignore[assignment]
    try:
        await runner.consume_result_batches(
            engine_registry=object(),  # type: ignore[arg-type] — not read (streams injected)
            async_w3=w3,  # type: ignore[arg-type]
            executor_address="0x" + "0" * 40,
            operator_address="0x" + "0" * 40,
            operator_private_key="0x" + "0" * 64,
            dispatcher=dispatcher,
            dry_run=True,
            block_stream=_Blocks(blocks),
            result_iter=_Results(batches),
        )
    finally:
        runner.dispatch_profitable_results = orig  # type: ignore[assignment]
    setattr(dispatcher, "_dispatched", dispatched)  # noqa: SLF001 — test fixture
    return dispatcher, w3


class TestBlockClockFromStream:
    async def test_block_clock_tracks_block_stream_not_solve_block(self) -> None:
        # Block stream advances 101 → 102 → 103. Result batches carry a stale
        # solve_block=999 to prove the clock ignores it.
        dispatcher, _w3 = await _run(
            blocks=[_block(101), _block(102), _block(103)],
            batches=[_empty_batch(999), _empty_batch(999)],
        )
        assert dispatcher.current_block == 103, (
            "block clock must track the block stream, not solve_block"
        )

    async def test_fee_history_keys_off_block_stream_numbers(self) -> None:
        # fee_history(newest_block=…) must use the block-stream number so the
        # consumer queries the right block's reward percentiles (the prior
        # implementation queried solve_block — a stale/wrong block).
        dispatcher, w3 = await _run(
            blocks=[_block(201), _block(202)],
            batches=[],
        )
        assert w3.eth.fee_history_blocks == [201, 202], (
            "fee_history must be called with the block-stream numbers"
        )

    async def test_dispatch_keys_off_block_stream_current_block(self) -> None:
        # A non-empty batch arrives with a deliberately-stale solve_block=999.
        # dispatch must use the dispatcher's CURRENT block (the block-stream
        # clock), NEVER the batch's solve_block — regardless of whether the
        # block stream has ticked past the batch's block yet (racing is
        # expected under asyncio.wait; the load-bearing claim is that the
        # batch's solve_block is never used as the clock).
        batch = dict(_empty_batch(999))
        batch["fresh"] = [(1, 100, 50, (1, 2), (3,))]  # one profitable result
        dispatcher, _w3 = await _run(
            blocks=[_block(301), _block(302)],
            batches=[batch],
        )
        dispatched: list[int] = getattr(dispatcher, "_dispatched")
        assert len(dispatched) == 1
        # The load-bearing contract: dispatch must NEVER use the batch's
        # solve_block (999) as the current block — only the block-stream
        # clock (the seed 0 or a ticked 301/302, depending on race order).
        assert dispatched[0] != 999, (
            "dispatch must key off the block-stream clock, never the batch's "
            "stale solve_block"
        )