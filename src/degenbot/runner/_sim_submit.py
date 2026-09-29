"""Ordered sim-submit pipeline wiring over the Rust mechanism.

The bounded concurrent sim fan-out, the single FIFO submit lane, and the
loud-abort contract are Rust-owned (``degenbot._ffi.submission.SimSubmitPipeline``
over ``degenbot-submission::sim_pipeline``). This module supplies only the two
Python async leaves and the factory that injects the configured in-flight cap:

- :func:`_run_sim` — candidate shaping, the GIL-free FFI sim, and the payload
  merge (the simulate leaf);
- :func:`_submit_ordered` — render, one nonce fetch at submit time, submit
  (the ordered lane step).

Submission order is nonce order because the Rust lane drains batches in
arrival order and awaits each batch's own sim before submitting. The cap VALUE
(``DEGENBOT_SIM_PIPELINE_CONCURRENCY``) stays driver-side as a plain ``usize``.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.dispatch import SimSubmitPipeline, dispatch_profitable
from degenbot.logging import logger as bot_logger
from degenbot.runner._dispatch import (
    _build_dispatch_candidates,
    _merge_payload_outcome,
    _render_outcome,
    _submit_batch_records,
)

if TYPE_CHECKING:
    from degenbot.runner._dispatch import RawEngineResult
    from degenbot.runner._render import _SimOutcome
    from degenbot.runner.bot_runner import _SessionState


@dataclass
class BatchWork:
    """One streamed solver batch traversing the pipeline."""

    results: list[RawEngineResult]
    block_timestamp: int
    base_fee_next: int
    current_block: int
    payloads: dict[int, dict] | None = None


async def _run_sim(session: _SessionState, work: BatchWork) -> _SimOutcome | None:
    """The simulate leaf for one batch (the FFI sim releases the GIL)."""
    # Candidate shaping runs on this task (same engine lock order as the
    # serial reference - engine-then-core via path_info_for_core). Payload
    # entries skip the FFI sim: the engine already simulated them inline.
    candidates = _build_dispatch_candidates(session, work.results, payloads=work.payloads)
    outcome = None
    if candidates:
        sim_ctx = session.sim_ctx
        if sim_ctx is None:
            msg = (
                "SimulateContext is required to dispatch"
                " (non-Alloy provider or sim context unbuilt)"
            )
            raise RuntimeError(msg)
        outcome = await dispatch_profitable(
            candidates=candidates,
            context=sim_ctx,
            dispatcher=session.dispatcher,
            base_fee_next=work.base_fee_next,
            current_block=work.current_block,
            block_timestamp=work.block_timestamp,
            min_profit_margin_bps=session.cfg.min_profit_margin_bps,
            engine=session.engine_registry.engine,
        )
    merged = _merge_payload_outcome(session, outcome, work.payloads)
    if not merged:
        bot_logger.debug("[sim-none] batch produced no dispatchable candidates")
    return merged


async def _submit_ordered(
    session: _SessionState,
    work: BatchWork,
    outcome: _SimOutcome,
) -> None:
    """Render + submit one completed batch outcome (the ordered lane step)."""
    _render_outcome(session, outcome, work.current_block)
    # The nonce fetch happens at SUBMIT time on the single ordered lane, so
    # the submission-time chain read is a fresh baseline for the Rust
    # authority's sign-time lease.
    operator_nonce = int(await session.async_w3.get_transaction_count(session.cfg.operator_address))
    submitted = _submit_batch_records(
        session,
        outcome,
        operator_nonce=operator_nonce,
    )
    # Test seams (and any future sync fallback) may return a plain value
    # instead of a coroutine - tolerate BOTH, never treat None as awaitable.
    if asyncio.iscoroutine(submitted):
        await submitted


def build_sim_submit_pipeline(
    session: _SessionState,
    *,
    concurrency: int | None = None,
) -> SimSubmitPipeline:
    """Build the FFI pipeline over this session's two Python async leaves.

    ``concurrency`` is the injected in-flight cap; omitted, the session's
    resolved config value is used. The resolved cap arrives as a value (the
    construction boundary resolved it once), never from a module-level verdict.
    """

    cap = concurrency if concurrency is not None else session.cfg.sim_pipeline_concurrency

    async def sim(work: BatchWork) -> _SimOutcome | None:
        return await _run_sim(session, work)

    async def submit(work: BatchWork, outcome: _SimOutcome) -> None:
        await _submit_ordered(session, work, outcome)

    return SimSubmitPipeline(sim=sim, submit=submit, concurrency=cap)
