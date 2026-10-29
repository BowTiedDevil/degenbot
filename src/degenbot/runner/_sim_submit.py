"""Batch-executor construction for the settlement-arbitrage cockpit.

The per-batch choreography — candidate assembly, the GIL-free sim fan-out,
the payload merge, the ordered nonce-serial serialization — is the Rust core
(``degenbot-batch-executor``) behind ``degenbot._ffi.simulation.BatchExecutor``.
This module is the CONSTRUCTION boundary only: it hands the FFI the session's
runtime handles and the FFI converts the policy values (the in-flight cap,
the thin-margin floor, the ERC6909 encode axis, the dry-run/inject guards)
from the installed verdict through ONE typed conversion — the conversion owns
every default and clamp, so this module resolves no policy value and carries
no twin floor. The driver's remaining responsibility is display: the consumer
drains the Batch outcome records (:meth:`~degenbot.runner._render.MergedOutcome.from_records`
is the render fold).

The boundary reads ONE :class:`ExecutorInputs` record (resolved from a session
by :meth:`ExecutorInputs.from_session`), never the session itself, so the
constructor's inputs are named in one place.

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

    from degenbot.dispatch import Dispatcher, SimulateContext
    from degenbot.provider import AsyncAlloyProvider
    from degenbot.runner._dispatch import RawEngineResult
    from degenbot.runner._relay_posture import RelayPosture
    from degenbot.runner.config import ArbitrageConfig
    from degenbot.runner.session import Session


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


@dataclass(frozen=True)
class ExecutorInputs:
    """The values one session hands the core executor's constructor.

    Resolved once by :meth:`from_session` at the pipeline seam, so the
    construction boundary reads named fields rather than a session bag.
    """

    sim_ctx: SimulateContext
    dispatcher: Dispatcher
    engine: Any
    provider: AsyncAlloyProvider
    cfg: ArbitrageConfig
    relay_posture: RelayPosture | None = None

    @classmethod
    def from_session(cls, session: Session) -> ExecutorInputs:
        """Resolve the boundary's inputs off one started session.

        Args:
            session: The session whose handles the executor is built over.

        Returns:
            The resolved construction inputs.

        Raises:
            RuntimeError: If the session has no sim context (a non-Alloy
                provider or a session that never built one).

        """
        sim_ctx = session.sim_ctx
        if sim_ctx is None:
            msg = (
                "SimulateContext is required to dispatch "
                "(non-Alloy provider or sim context unbuilt)"
            )
            raise RuntimeError(msg)
        actors = session.actors
        dispatcher = session.dispatcher
        assert actors is not None, "the executor boundary needs a started session"
        assert dispatcher is not None, "the executor boundary needs a session dispatcher"
        return cls(
            sim_ctx=sim_ctx,
            dispatcher=dispatcher,
            engine=actors.engine_registry.engine,
            provider=actors.async_w3,
            cfg=session.cfg,
            relay_posture=session.relay_posture,
        )


async def build_batch_executor(
    inputs: ExecutorInputs,
    *,
    engine_builder: Callable[..., BatchExecutor] | None = None,
) -> BatchExecutor:
    """Build the core batch executor over one session's resolved policy.

    Every policy value (thin-margin floor, the ERC6909 encode axis, the
    in-flight cap, the inject guards) converts from the installed verdict
    inside the FFI; the driver injects only the runtime handles the verdict
    cannot name (the run-mode stance, the relay posture's broadcast fan-out,
    the nonce seed). The construction-time chain read seeds the Rust nonce
    authority's lane; the hosted per-head reconcile maintains it afterwards.

    ``engine_builder`` is the construction seam: a caller injects its own
    constructor to observe exactly which values cross this boundary. The
    production default is the companion package's FFI ``build_batch_executor``,
    so the uninjected path stays Rust-bound.

    Args:
        inputs: The construction boundary's resolved inputs.
        engine_builder: The construction override; defaults to the FFI
            ``build_batch_executor``.

    Returns:
        The constructed batch executor.

    Raises:
        RuntimeError: If the session's provider is not Alloy-backed (nothing
            can submit).

    """
    async_alloy = inputs.provider.as_async_alloy()
    if async_alloy is None:
        msg = "async_w3 is not an Alloy-backed provider; cannot submit"
        raise RuntimeError(msg)

    # The relay posture value resolved once at session start (see
    # _relay_posture): its endpoints are this session's broadcast fan-out.
    relay_posture = inputs.relay_posture
    broadcast_providers = (
        await _resolve_relay_providers(relay_posture.relay_urls, None)
        if relay_posture is not None and relay_posture.relay_urls
        else None
    )

    operator_nonce = int(await inputs.provider.get_transaction_count(inputs.cfg.operator_address))
    signer = TxSigner(key=inputs.cfg.operator_private_key, chain_id=inputs.cfg.chain_id)
    construct = engine_builder or build_batch_executor_py
    return construct(
        context=inputs.sim_ctx,
        dispatcher=inputs.dispatcher,
        engine=inputs.engine,
        signer=signer,
        submit_provider=async_alloy,
        operator_nonce=operator_nonce,
        dry_run=inputs.cfg.dry_run,
        broadcast_providers=broadcast_providers,
    )


async def build_executor_for(session: Session) -> BatchExecutor:
    """Build the core batch executor over one session (the pipeline factory).

    Args:
        session: The session whose handles the executor is built over.

    Returns:
        The constructed batch executor.

    """
    return await build_batch_executor(ExecutorInputs.from_session(session))
