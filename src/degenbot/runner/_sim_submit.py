"""Batch-executor construction for the settlement-arbitrage cockpit.

The per-batch choreography — candidate assembly, the GIL-free sim fan-out,
the payload merge, the ordered nonce-serial submission — is the Rust core
(``degenbot-batch-executor``) behind ``degenbot._ffi.simulation.BatchExecutor``.
This module is the CONSTRUCTION boundary only: it resolves the policy values
once (the in-flight cap, the thin-margin floor, the relay posture's broadcast
fan-out, the dry-run/inject guards) and builds the executor. The driver's
remaining responsibility is display: the consumer drains the Batch outcome
records (:meth:`~degenbot.runner._dispatch.MergedOutcome.from_records` is the
render fold).

Submission order is nonce order because the core lane drains batches in
arrival order (FIFO). The cap VALUE (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``,
default 8, floored at 1) stays driver-side as a plain ``usize`` — resolved
here, injected at construction, never re-read below.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.dispatch import BatchExecutor, TxSigner
from degenbot.dispatch import (
    build_batch_executor as build_batch_executor_py,
)
from degenbot.runner._dispatch import _resolve_relay_providers

if TYPE_CHECKING:
    from degenbot.runner._dispatch import RawEngineResult
    from degenbot.runner.bot_runner import _SessionState


@dataclass
class BatchWork:
    """One streamed solver batch (the FFI enqueue payload).

    The executor seam extracts this by field name (the frozen-dataclass
    getattr pattern); ``payloads`` keys are the batch-local payload-served
    set. ``current_block`` is also the dispatch-clock value the consumer
    stub records.
    """

    results: list[RawEngineResult]
    block_timestamp: int
    base_fee_next: int
    current_block: int
    payloads: dict[int, dict] | None = None


def resolve_sim_concurrency(
    session: _SessionState,
    *,
    concurrency: int | None = None,
) -> int:
    """The injected in-flight sim cap: the explicit value, else the config.

    A cap of zero sims would wedge the pipeline, not serialize it — floored
    at 1 (the config resolver already floors; the floor re-applies here so an
    injected value obeys the same rule).
    """
    cap = concurrency if concurrency is not None else session.cfg.sim_pipeline_concurrency
    return max(int(cap), 1)


async def build_batch_executor(
    session: _SessionState,
    *,
    concurrency: int | None = None,
) -> BatchExecutor:
    """Build the core batch executor over this session's resolved policy.

    ``concurrency`` is the injected in-flight cap; omitted, the session's
    resolved config value is used. Every other policy value rides the
    session's resolved config (thin-margin floor, the ERC6909 encode axis,
    the dry-run/inject guards) and the resolved relay posture (the broadcast
    fan-out — endpoints dialed ONCE here and cached module-side). The
    construction-time chain read seeds the Rust nonce authority's lane; the
    hosted per-head reconcile maintains it afterwards.
    """
    cap = resolve_sim_concurrency(session, concurrency=concurrency)
    sim_ctx = session.sim_ctx
    if sim_ctx is None:
        msg = "SimulateContext is required to dispatch (non-Alloy provider or sim context unbuilt)"
        raise RuntimeError(msg)
    async_alloy = session.async_w3.as_async_alloy()
    if async_alloy is None:
        msg = "async_w3 is not an Alloy-backed provider; cannot submit"
        raise RuntimeError(msg)

    # The relay posture value resolved once at session start (see
    # _relay_posture): its endpoints are this session's broadcast fan-out.
    relay_posture = getattr(session, "relay_posture", None)
    broadcast_providers = (
        await _resolve_relay_providers(relay_posture.relay_urls, None)
        if relay_posture is not None and relay_posture.relay_urls
        else None
    )

    operator_nonce = int(await session.async_w3.get_transaction_count(session.cfg.operator_address))
    signer = TxSigner(key=session.cfg.operator_private_key, chain_id=session.cfg.chain_id)
    return build_batch_executor_py(
        context=sim_ctx,
        dispatcher=session.dispatcher,
        engine=session.engine_registry.engine,
        signer=signer,
        submit_provider=async_alloy,
        operator_nonce=operator_nonce,
        sim_concurrency=cap,
        min_profit_margin_bps=session.cfg.min_profit_margin_bps,
        dry_run=session.cfg.dry_run,
        inject_code_guard=session.cfg.inject_executor_code,
        erc6909_profit=session.cfg.erc6909_profit,
        max_candidates=0,
        broadcast_providers=broadcast_providers,
    )
