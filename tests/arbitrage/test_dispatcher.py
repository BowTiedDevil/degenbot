"""Unit tests for the Dispatcher — the coordination-state value object.

`Dispatcher` bundles the seven mutable containers that `main()` previously
declared as exploded locals and threaded through `consume_result_batches`,
`dispatch_profitable_results`, and `monitor_pending_transaction`: pending
nonces/pools, in-flight tasks, the current-block reference, the block-time
deque, per-block priority fees, and (composed) the `PathSuppression`.

These tests cover the pure coordination logic (nonce claiming, pool mutual
exclusion, task tracking, block/fee recording) without a live engine, WS, or
RPC — the consume/dispatch functions are exercised only by the example import.
"""

from __future__ import annotations

import asyncio

import pytest

import examples.eth_backrun_v2_v3_v4_rust as runner
from examples.eth_backrun_v2_v3_v4_rust import Dispatcher


class TestClaimNonce:
    def test_claim_nonce_returns_start_when_nothing_pending(self) -> None:
        d = Dispatcher.for_block(100)
        assert d.claim_nonce(5) == 5
        assert 5 in d.pending_nonces

    def test_claim_nonce_skips_pending_nonces(self) -> None:
        d = Dispatcher.for_block(100)
        # 5 and 6 already in flight — must claim 7
        d.pending_nonces.update({5, 6})
        assert d.claim_nonce(5) == 7
        assert d.pending_nonces == {5, 6, 7}


class TestRelease:
    def test_release_nonce_discards(self) -> None:
        d = Dispatcher.for_block(100)
        nonce = d.claim_nonce(3)
        assert nonce == 3
        d.release_nonce(nonce)
        assert nonce not in d.pending_nonces

    def test_release_pools_difference_updates(self) -> None:
        d = Dispatcher.for_block(100)
        d.reserve_pools({10, 20, 30})
        d.release_pools({20, 30})
        assert d.pending_pools == {10}


class TestIsPathBlocked:
    def test_unblocked_when_no_overlap(self) -> None:
        d = Dispatcher.for_block(100)
        d.reserve_pools({1, 2})
        assert d.is_path_blocked({5, 6}, committed_pools=set()) is False

    def test_blocked_by_pending_pool(self) -> None:
        d = Dispatcher.for_block(100)
        d.reserve_pools({1, 2})
        assert d.is_path_blocked({2, 5}, committed_pools=set()) is True

    def test_blocked_by_committed_pool(self) -> None:
        d = Dispatcher.for_block(100)
        # nothing pending, but pool 3 already committed this batch
        assert d.is_path_blocked({3}, committed_pools={3}) is True


class TestReleaseTx:
    def test_release_tx_frees_nonce_and_pools(self) -> None:
        d = Dispatcher.for_block(100)
        nonce = d.claim_nonce(7)
        d.reserve_pools({11, 12})
        tx = runner.SubmittedTx(
            tx_hash=b"\x00" * 32,
            nonce=nonce,
            pools={11, 12},
            submission_block=100,
        )
        d.release_tx(tx)
        assert nonce not in d.pending_nonces
        assert d.pending_pools == set()


class TestTrackTask:
    def test_track_task_registers_and_auto_discards(self) -> None:
        d = Dispatcher.for_block(100)

        async def noop() -> None:
            pass

        loop = asyncio.new_event_loop()
        try:
            task = loop.create_task(noop())
            d.track_task(task)
            assert task in d.active_tasks
            loop.run_until_complete(task)
            # done_callback runs after completion; it discards the task
            assert task not in d.active_tasks
        finally:
            loop.close()


class TestBlockAndFeeRecording:
    def test_current_block_round_trip(self) -> None:
        d = Dispatcher.for_block(100)
        assert d.current_block == 100
        d.advance_block(105)
        assert d.current_block == 105

    def test_block_times_appends_with_maxlen(self) -> None:
        d = Dispatcher.for_block(0)
        for i in range(70):
            d.record_block_time(i, i * 12)
        # deque maxlen=60 — only the last 60 survive
        assert len(d.block_times) == 60
        assert d.block_times[-1] == (69, 69 * 12)
        assert d.block_times[0] == (10, 120)

    def test_latest_priority_fees_returns_max_block(self) -> None:
        d = Dispatcher.for_block(0)
        d.record_priority_fees(100, {50: 1_000})
        d.record_priority_fees(110, {50: 2_000})
        d.record_priority_fees(105, {50: 1_500})  # out-of-order insert
        assert d.latest_priority_fees() == {50: 2_000}

    def test_latest_priority_fees_empty_raises(self) -> None:
        d = Dispatcher.for_block(0)
        with pytest.raises(ValueError, match="is empty"):
            d.latest_priority_fees()


def test_dispatcher_composes_path_suppression() -> None:
    """Dispatcher delegates to a real PathSuppression instance."""
    d = Dispatcher.for_block(100)
    # record enough failures to trip the threshold
    threshold = runner.PATH_SUPPRESS_THRESHOLD
    for _ in range(threshold):
        d.record_failure(42)
    assert d.is_suppressed(42, current_block=100) is False  # first retry is allowed
    # a fresh path is not suppressed
    assert d.is_suppressed(99, current_block=100) is False
