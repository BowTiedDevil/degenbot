"""T7 (ADR-006 D4): hot-loop recurring liquidity-map verification.

The single batch ``verify_liquidity_maps`` at startup step 3b runs once,
pre-loop. Anything desyncing after it — during ``release_python_state`` or in
the pump (e.g. the V3 unregister bug at ``3ae6fa04``) — is never detected
without a recurring check. This module drives ``verify_liquidity_maps`` every
``interval`` blocks so such desyncs surface within the interval and halt the
bot (fail-fast — never trade past a detected desync).

Kept in ``degenbot.arbitrage`` (not the example) so it is importable by tests
and reusable by any operator; the example wires it into its main loop.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
    retry_verification_call_async,
)

if TYPE_CHECKING:
    import asyncio
    from collections.abc import AsyncIterator

logger = logging.getLogger(__name__)

# Default interval: bounds RPC cost. A post-release / in-loop desync is caught
# within this window. Operators tune via the example's RECURRING_VERIFY_INTERVAL.
RECURRING_VERIFY_INTERVAL_DEFAULT = 50


@runtime_checkable
class _Registers(Protocol):
    """The verify surface ``EngineRegistry`` exposes — a Protocol so this module
    doesn't import ``engine_registry`` (which imports ``verification_retry``)."""

    async def verify_liquidity_maps(self, *, block_number: int | None) -> None: ...


async def run_recurring_verify_until_done(  # noqa: C901 - linear, not complex
    *,
    registry: _Registers,
    block_ticker: "AsyncIterator[int]",
    interval: int,
    retry_policy: VerificationRetryPolicy,
) -> None:
    """Drive a recurring ``verify_liquidity_maps`` alongside the hot loop.

    Every ``interval`` blocks (where ``block % interval == 0``), calls
    ``registry.verify_liquidity_maps(block_number=block)`` (the relocated PyBot
    batch API) and emits ``[verify] (recurring)`` lines — distinguished from the
    startup batch.

    A mismatch raises fail-fast (same RuntimeError category as the batch),
    halting the bot. Transient RPC transport failures retry-with-backoff via
    the ``VerificationRpcError`` policy; genuine mismatches do not retry.

    Args:
        registry: anything exposing ``async def verify_liquidity_maps(block_number=...)``.
        block_ticker: an async iterator yielding block numbers as the pump
            accepts them (e.g. the engine's ``block_stream()``).
        interval: verify every N blocks. ``<= 0`` disables recurring verify.
        retry_policy: transient RPC failure retry policy.
    """
    async for block in block_ticker:
        if interval <= 0 or block % interval != 0:
            continue
        logger.info("[verify] (recurring) checking at block %d", block)
        # retry transient RPC transport errors; mismatch propagates fail-fast.
        await retry_verification_call_async(
            retry_policy,
            registry.verify_liquidity_maps,
            block_number=block,
        )
        logger.info("[verify] (recurring) V3 + V4 liquidity maps OK at block %d", block)