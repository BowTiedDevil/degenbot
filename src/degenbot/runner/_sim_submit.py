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

An explicit execution probe does NOT ride this constructor. It has its own
boundary (:func:`build_probe_executor`) over the core's dedicated, correlated
``ProbeExecutor``: no signer, no nonce seed, no submit provider, no relay
fan-out, and no run-mode stance cross it, because a probe has no submit lane to
arm. The probe's capital posture still crosses as ONE value
(:class:`ProbeCapital`), which the FFI converts once into the core's capital
mode (the stream's funding axis AND the deployment's simulation override plan).

Submission order is nonce order because the core lane drains batches in
arrival order (FIFO). The cap VALUE (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``,
declared default 8, floored at 1 by the conversion) lives in the verdict —
``degenbot.config.resolved_config().values.simulation.pipeline_concurrency``
reads the same answer the executor was built from.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING

from degenbot.dispatch import BatchExecutor, ProbeExecutor, SessionConsumer, TxSigner
from degenbot.dispatch import (
    build_batch_executor as build_batch_executor_py,
)
from degenbot.dispatch import build_probe_executor as build_probe_executor_py
from degenbot.runner._dispatch import _resolve_relay_providers

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import Any

    from degenbot.dispatch import Dispatcher, SimulateContext, SimulationDeployment
    from degenbot.provider import AsyncAlloyProvider
    from degenbot.runner._relay_posture import RelayPosture
    from degenbot.runner.config import ArbitrageConfig
    from degenbot.runner.session import Session


class ProbeCapital(Enum):
    """The capital posture one executor construction asks for.

    ONE value names what the executor assumes about its own capital, and the
    core derives both halves from it: the stream's funding axis and the
    simulation override plan the deployment applies to its execution target.

    The two probe modes are mutually exclusive by construction — they are
    members of one enum, not two booleans — and the spellings are the same
    vocabulary the operator wire and the FFI accept.
    """

    #: The deployed contract's own capital: production encoding, and no
    #: simulation override at all.
    PRODUCTION = "production"
    #: Executor-held capital from the known asset set, with the self-funded
    #: stream encoding.
    SELF_FUND = "self_fund"
    #: The known asset set stripped from the executor, with the in-path-flash
    #: stream encoding.
    ZERO_BALANCE = "zero_balance"


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
    #: The session's ADDITIONAL execution deployments (foreign contracts),
    #: registered after the built-in settlement deployment in this order. Empty
    #: is the one-entry built-in session (:meth:`from_session` leaves it empty:
    #: no verdict key declares a foreign deployment yet), so a driver that has
    #: some names them in its own pipeline factory. The fan-out choreography,
    #: the identity/authority checks, and the per-target joins stay in the Rust
    #: core.
    deployments: tuple[SimulationDeployment, ...] = ()
    #: Which deployments each solved path is offered to: ``"first_compatible"``
    #: (the default) walks the registry in order and executes at the first
    #: deployment whose composition accepts the path, or ``"all_targets"`` for
    #: the explicit fan-out to every registered deployment. The walk itself is
    #: the Rust core's; this field only names the policy.
    target_selection: str = "first_compatible"
    #: The ONE capital posture a PROBE construction assumes (:class:`ProbeCapital`).
    #: It is read by :func:`build_probe_executor` only — the submitting
    #: constructor always simulates the session account's real balances
    #: (``Production``), and a probe names its mode here, in ONE place
    #: (see ``Session.probe_path``).
    capital: ProbeCapital = ProbeCapital.PRODUCTION

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

    The run mode and the relay posture decide the submit stance: there is no
    stance argument to pass, because a submitting lane is the only thing this
    constructor builds. An explicit execution probe is
    :func:`build_probe_executor`'s value, which has no submit lane at all.

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
    # _relay_posture): its endpoints are this construction's broadcast fan-out.
    relay_posture = inputs.relay_posture
    relay_urls = list(relay_posture.relay_urls) if relay_posture is not None else []
    broadcast_providers = await _resolve_relay_providers(relay_urls, None) if relay_urls else None

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
        deployments=list(inputs.deployments) or None,
        target_selection=inputs.target_selection,
    )


def build_probe_executor(
    inputs: ExecutorInputs,
    *,
    probe_builder: Callable[..., ProbeExecutor] | None = None,
) -> ProbeExecutor:
    """Build a correlated, NON-SUBMITTING execution probe executor.

    The boundary is the sim half of :func:`build_batch_executor`'s and nothing
    else. NOTHING submission-shaped crosses it: no ``TxSigner`` is minted from
    the operator key, no ``get_transaction_count`` seeds a nonce lane, no
    ``submit_provider`` is handed over, no relay endpoint is dialed, and no
    ``dry_run`` stance is passed — the core's probe construction cannot name a
    signer, a nonce lease, or a broadcast target at all, so non-submission is a
    missing field rather than a flag this function sets.

    The capital posture rides ``inputs.capital`` (one :class:`ProbeCapital`
    value) and is handed to the FFI as its wire spelling, so a caller states the
    posture in ONE place.

    ``probe_builder`` is the construction seam: a caller injects its own
    constructor to observe exactly which values cross this boundary. The default
    is the companion package's FFI ``build_probe_executor``, so the uninjected
    path stays Rust-bound.

    Args:
        inputs: The construction boundary's resolved inputs (its ``capital`` is
            the probe's ONE posture).
        probe_builder: The construction override; defaults to the FFI
            ``build_probe_executor``.

    Returns:
        The constructed probe executor (one per capital posture).

    Raises:
        RuntimeError: If the session's provider is not Alloy-backed (the sim
            stage needs one).

    A refused deployment registry surfaces as the FFI's ``ValueError``.

    """
    async_alloy = inputs.provider.as_async_alloy()
    if async_alloy is None:
        msg = "async_w3 is not an Alloy-backed provider; cannot simulate"
        raise RuntimeError(msg)

    construct = probe_builder or build_probe_executor_py
    return construct(
        context=inputs.sim_ctx,
        dispatcher=inputs.dispatcher,
        engine=inputs.engine,
        provider=async_alloy,
        capital=inputs.capital.value,
        deployments=list(inputs.deployments) or None,
        target_selection=inputs.target_selection,
    )


async def build_executor_for(session: Session) -> BatchExecutor:
    """Build the core batch executor over one session (the pipeline factory).

    Args:
        session: The session whose handles the executor is built over.

    Returns:
        The constructed batch executor.

    """
    return await build_batch_executor(ExecutorInputs.from_session(session))


def build_session_consumer(session: Session, executor: BatchExecutor) -> SessionConsumer:
    """Build the session's core event consumer over its engine's one hand-off.

    The consumer is the loop: it takes the engine's ordered stream and the claim
    that proves a consumer holds it, derives each batch's work, enqueues it on
    the session's executor, and owns the loud-abort rule and the session close.
    Python keeps only the drain (``_consume``) and the error/calling-convention
    translation.

    The session is supervised: a natural stream end means the pump died outside
    this session's own teardown, so the end verdict is the loud
    ``pump-stopped``. The advisory per-header ``eth_feeHistory`` sample rides the
    same loop (the core's declared percentile pair), where the pre-cut-over
    driver awaited it per block.

    Args:
        session: The started session the consumer is built over.
        executor: The session's constructed batch executor.

    Returns:
        The attached session consumer (its ``resume`` arms the pump).

    """
    actors = session.actors
    assert actors is not None, "the session consumer is built by a started session"
    consumer = SessionConsumer(
        executor,
        allow_quiet_end=False,
        fee_history_provider=actors.async_w3.as_async_alloy(),
    )
    consumer.attach(actors.engine_registry.engine)
    return consumer
