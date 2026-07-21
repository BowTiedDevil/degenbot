"""T7 (ADR-006 D4): hot-loop recurring verify_liquidity_maps.

Post-release / in-loop desyncs (e.g. the V3 unregister bug at 3ae6fa04) develop
AFTER the single batch verify at step 3b passes — during `release_python_state`
or in the hot loop. Without a recurring verify the bot trades on stale data.
The recurring verify surfaces such desyncs within N blocks and halts (fail-fast).
"""

from __future__ import annotations

import asyncio

import pytest

from degenbot.arbitrage.verification_retry import VerificationRetryPolicy


class _FakeRegistry:
    """Records verify_liquidity_maps calls; can be made to fail."""

    def __init__(self) -> None:
        self.verify_calls: list[int | None] = []
        self.fail_at: int | None = None  # fail on the Nth verify call's block

    async def verify_liquidity_maps(self, *, block_number: int | None = None) -> None:
        self.verify_calls.append(block_number)
        if self.fail_at is not None and block_number == self.fail_at:
            raise RuntimeError(f"[verify] (recurring) MISMATCH at block {block_number}")


async def _drive_recurring_verify(
    registry: _FakeRegistry,
    block_sequence: list[int],
    interval: int,
) -> None:
    """A minimal harness mirroring the example's recurring-verify loop: ticks a
    block clock, and every `interval` blocks calls verify_liquidity_maps(block)."""
    from degenbot.arbitrage.recurring_verify import run_recurring_verify_until_done

    # block_sequence: the loop calls a block-ticker that yields each block.
    async def _block_ticker():
        for b in block_sequence:
            yield b

    await run_recurring_verify_until_done(
        registry=registry,
        block_ticker=_block_ticker(),
        interval=interval,
        retry_policy=VerificationRetryPolicy(),
    )


def test_recurring_verify_runs_every_interval_blocks() -> None:
    """The recurring verify runs every N blocks (default 50), tagged
    '(recurring)' — distinguishable from the startup batch. Verifies at the
    engine's last_processed_block for determinism."""
    registry = _FakeRegistry()

    # blocks 1..120 with interval 50 → verify at blocks 50 and 100.
    asyncio.run(_drive_recurring_verify(registry, list(range(1, 121)), interval=50))

    assert registry.verify_calls == [50, 100], (
        f"expected verifies at 50 and 100, got {registry.verify_calls}"
    )


def test_recurring_verify_fail_fast_halts_loop() -> None:
    """A post-release desync (simulated mismatch at block 100) surfaces via the
    recurring verify and halts the bot within the interval — never trades past
    a detected desync."""
    registry = _FakeRegistry()
    registry.fail_at = 100

    with pytest.raises(RuntimeError, match="\\(recurring\\) MISMATCH at block 100"):
        asyncio.run(_drive_recurring_verify(registry, list(range(1, 121)), interval=50))

    # The mismatch halted the loop at block 100 — did NOT continue to 120.
    assert registry.verify_calls == [50, 100]
