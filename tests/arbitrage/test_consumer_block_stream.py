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

from typing import Any

import pytest

from degenbot.dispatch import Dispatcher
from degenbot.runner._consume import consume_result_batches


class _Eth:
    """Fake eth namespace — records fee_history blocks + nonce."""

    def __init__(self, nonce: int = 5) -> None:
        self._nonce = nonce
        self.fee_history_blocks: list[int] = []

    # PAGQCK: the dispatch hot loop now routes ``eth_feeHistory`` via
    # ``make_request`` (raw JSON shape with hex-string rewards) instead of
    # ``async_w3.eth.fee_history(block_count=, newest_block=, ...)``. The
    # fake records the requested ``newest_block`` from the make_request params
    # (``[block_count, hex(newest_block), percentiles]``) + returns the empty-
    # reward shape the example expects (no priority-fee recording).
    async def fee_history(self, *, block_count: int, newest_block: int, reward_percentiles):
        self.fee_history_blocks.append(int(newest_block))
        return {"reward": [[]]}  # empty reward → no priority-fee recording

    async def get_transaction_count(self, address: str) -> int:
        return self._nonce


class _FakeW3:
    """Fake ``AsyncAlloyProvider`` for the dispatch-path tests (PAGQCK).

    The dispatch hot loop was routed off raw ``AsyncWeb3`` onto
    ``AsyncAlloyProvider`` — this fake exposes the SAME flat surface
    (``get_transaction_count`` / ``make_request`` / ``rpc_url``) the example
    now drives, instead of the old ``async_w3.eth.<method>`` nesting. The
    ``make_request`` dispatcher mirrors the raw-RPC shapes the alloy backend
    returns (hex strings; ``eth_feeHistory`` returns the empty-reward dict).
    """

    def __init__(self) -> None:
        self.eth = _Eth()  # drives fee_history/get_transaction_count tracking

    async def get_transaction_count(self, address: str) -> int:
        return await self.eth.get_transaction_count(address)

    async def make_request(self, method: str, params: list):
        if method == "eth_feeHistory":
            # The params list is [block_count, hex(newest_block), percentiles].
            return await self.eth.fee_history(
                block_count=int(params[0]),
                newest_block=int(params[1], 16),
                reward_percentiles=params[2],
            )
        msg = f"_FakeW3.make_request: unhandled method {method!r}"
        raise NotImplementedError(msg)

    @property
    def rpc_url(self) -> str:
        return "http://fake:8545"

    def as_async_alloy(self):
        # The PyO3 submission seam extracts a real `AsyncAlloyProvider` from
        # the adapter to drive the Rust `fetch_fee_history_py` leaf. The test
        # fake has no alloy backend — returning ``None`` makes `_apply_block_
        # if_ready` skip the fee-history leaf (the RPC parity is now exercised
        # by the Rust `fetch_fee_history` tests per §4.3, not this Python
        # mock).
        return None


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
) -> tuple[Dispatcher, _FakeW3, list[int]]:
    dispatcher = dispatcher or Dispatcher.for_block(0)
    w3 = _FakeW3()
    # Monkeypatch the consumer's binding of ``_dispatch_profitable`` so a
    # non-empty batch records the ``current_block`` it was dispatched with,
    # proving it keys off the block stream (not solve_block).
    dispatched: list[int] = []

    def _fake_dispatch(session: Any, results: Any, **kwargs: Any):
        dispatched.append(session.dispatcher.current_block)

        async def _n() -> None:
            pass

        return _n()

    from degenbot.runner import _consume as _runner_consume
    from degenbot.runner.bot_runner import _SessionState
    from degenbot.runner.config import ArbitrageConfig

    orig = _runner_consume._dispatch_profitable
    _runner_consume._dispatch_profitable = _fake_dispatch  # type: ignore[assignment]
    owner = _SessionState(
        engine_registry=object(),  # type: ignore[arg-type] — not read (streams injected)
        async_w3=w3,  # type: ignore[arg-type]
        sim_ctx=None,
        dispatcher=dispatcher,
        cfg=ArbitrageConfig.from_env(
            {
                "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
                "OPERATOR_PRIVATE_KEY": "0x"
                + "11" * 32,  # valid secp256k1 scalar, cosmetic (leaf stubbed)
                "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
                "INJECT_EXECUTOR_CODE": "0",
            },
            live=False,
            permutation=None,
            cli_http="http://localhost:8545",
            cli_ws="ws://localhost:8546",
        ),
        current_block=dispatcher.current_block,
    )
    try:
        await consume_result_batches(
            owner,
            block_stream=_Blocks(blocks),
            result_iter=_Results(batches),
            allow_quiet_end=allow_quiet_end,
        )
    finally:
        _runner_consume._dispatch_profitable = orig  # type: ignore[assignment]
    return dispatcher, w3, dispatched


class TestBlockClockFromStream:
    async def test_block_clock_tracks_block_stream_not_solve_block(self) -> None:
        # Block stream advances 101 → 102 → 103. Result batches carry a stale
        # solve_block=999 to prove the clock ignores it.
        dispatcher, _w3, _dispatched = await _run(
            blocks=[_block(101), _block(102), _block(103)],
            batches=[_empty_batch(999), _empty_batch(999)],
        )
        assert dispatcher.current_block == 103, (
            "block clock must track the block stream, not solve_block"
        )

    async def test_fee_history_keys_off_block_stream_numbers(self) -> None:
        # 7UIYJ6: the fee-history RPC + record-priority-fees now run in the
        # Rust submit leaf (`fetch_fee_history_py`), keyed off the block-stream
        # number passed by `_apply_block_if_ready`. The RPC-parity (that the
        # leaf queries the block-stream number's reward percentiles) is
        # exercised by the Rust `fetch_fee_history` tests per §4.3 — this Python
        # test now verifies only that the per-block advance drives the clock
        # + records block-time pairs (the latency-baseline ring the leaf feeds).
        dispatcher, _w3, _dispatched = await _run(
            blocks=[_block(201), _block(202)],
            batches=[],
        )
        assert dispatcher.current_block == 202, (
            "block clock must track the block stream through fee_history"
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
        ]  # one profitable result (state_nonces=(0,) — AV42C7)
        _dispatcher, _w3, dispatched = await _run(
            blocks=[_block(301), _block(302)],
            batches=[batch],
        )
        assert len(dispatched) == 1
        # The load-bearing contract: dispatch must NEVER use the batch's
        # solve_block (999) as the current block — only the block-stream
        # clock (the seed 0 or a ticked 301/302, depending on race order).
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
