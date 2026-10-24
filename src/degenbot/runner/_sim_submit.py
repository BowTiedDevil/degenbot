"""Batch-executor construction for the settlement-arbitrage cockpit.

The per-batch choreography — candidate assembly, the GIL-free sim fan-out,
the payload merge, the ordered nonce-serial submission — is the Rust core
(``degenbot-batch-executor``) behind ``degenbot._ffi.simulation.BatchExecutor``.
This module is the CONSTRUCTION boundary only: it hands the FFI the session's
runtime handles and the FFI converts the policy values (the in-flight cap,
the thin-margin floor, the ERC6909 encode axis, the dry-run/inject guards)
from the installed verdict through ONE typed conversion — the conversion owns
every default and clamp, so this module resolves no policy value and carries
no twin floor. The driver's remaining responsibility is display: the consumer
drains the Batch outcome records
(:meth:`~degenbot.runner._dispatch.MergedOutcome.from_records` is the render
fold).

Submission order is nonce order because the core lane drains batches in
arrival order (FIFO). The cap VALUE (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``,
declared default 8, floored at 1 by the conversion) lives in the verdict —
``degenbot.config.resolved_config().values.simulation.pipeline_concurrency``
reads the same answer the executor was built from.
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
    from collections.abc import Callable
    from typing import Any

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
    payloads: dict[int, dict[str, Any]] | None = None


async def build_batch_executor(
    session: _SessionState,
    *,
    engine_builder: Callable[..., BatchExecutor] | None = None,
) -> BatchExecutor:
    """Build the core batch executor over this session's resolved policy.

    Every policy value (thin-margin floor, the ERC6909 encode axis, the
    in-flight cap, the inject guards) converts from the installed verdict
    inside the FFI; the driver injects only the runtime handles the verdict
    cannot name (the run-mode stance, the relay posture's broadcast fan-out,
    the nonce seed). The construction-time chain read seeds the Rust nonce
    authority's lane; the hosted per-head reconcile maintains it afterwards.

    ``engine_builder`` is the construction seam: a caller injects its own
    constructor to observe exactly which values cross this boundary. The
    production default is the companion package's FFI
    ``build_batch_executor``, so the uninjected path stays Rust-bound.

    Args:
        session: The session whose runtime handles the executor is built over.
        engine_builder: The construction override; defaults to the FFI
            ``build_batch_executor``.

    Returns:
        The constructed batch executor.

    Raises:
        RuntimeError: If the session has no sim context or the provider is
            not Alloy-backed (nothing can submit).

    """
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
    construct = engine_builder or build_batch_executor_py
    return construct(
        context=sim_ctx,
        dispatcher=session.dispatcher,
        engine=session.engine_registry.engine,
        signer=signer,
        submit_provider=async_alloy,
        operator_nonce=operator_nonce,
        dry_run=session.cfg.dry_run,
        broadcast_providers=broadcast_providers,
    )
