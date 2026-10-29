"""The cockpit session: one owner of a pump session's coordination state.

A :class:`Session` is the ONE home for what a running pump session needs and
knows: the typed config, the resolved actors, the dispatcher, the block clock,
the sim context, the run-phase handles (the drain/reistration tasks and the
pipelines), the submission-smoke FSM, and the lifecycle FSM. Nothing is mirrored
on a facade — readers consult the session, and mutations ride its mutators.

The lifecycle is the Rust host's ``SessionPhase`` table (``strategy_host.rs``),
read through ``degenbot._ffi.session_phase_next``; :class:`SessionPhase`
translates that verdict and never authors the transition matrix, and
:class:`PhaseError` is its Python refusal surface.

The startup ordering itself is the run ritual (:mod:`~degenbot.runner._run_ritual`),
which takes a session — not a runner — and drives consumer-attach → watch-attach
→ ``resume()`` → registration → main loop over it.

:class:`~degenbot.runner.bot_runner.BotRunner` is the operator shell over one
session: ``start / run / enqueue_path / trigger_discovery / shutdown``.
"""

from __future__ import annotations

import asyncio
import contextlib
import gc
import inspect
import signal
from dataclasses import dataclass, field, replace
from enum import Enum
from typing import TYPE_CHECKING, Any, Self, cast

from degenbot.arbitrage import session_phase_next
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.dispatch import BatchExecutor, Dispatcher, SimulateContext, fetch_fee_history
from degenbot.logging import logger as bot_logger
from degenbot.runner._actors import (
    build_async_w3,
    build_bot,
    build_sim_ctx,
    mint_relay_posture,
    resolve_readiness,
)
from degenbot.runner._consume import BlockLoop, consume_result_batches
from degenbot.runner._dispatch import SubmissionSmoke
from degenbot.runner._render import DisplayPolicy
from degenbot.runner._run_ritual import RunRitual
from degenbot.runner._session_watch import SessionWatch
from degenbot.runner._sim_submit import build_executor_for
from degenbot.runner.build_paths import build_paths
from degenbot.runner.diag import arm_diagnostics
from degenbot.strategy import settlement_broadcast_endpoints, validate_strategy_readiness
from degenbot.uniswap.deployments import EthereumMainnetUniswapV4

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Coroutine

    from degenbot import Bot
    from degenbot.provider import AsyncAlloyProvider
    from degenbot.runner._relay_posture import RelayPosture
    from degenbot.runner._session_watch import SessionEndVerdict
    from degenbot.runner.build_paths import ConstructionContext
    from degenbot.runner.config import ArbitrageConfig
    from degenbot.strategy import StrategyReadinessView


class PhaseError(RuntimeError):
    """Cockpit phase violation: a lifecycle method ran in the wrong phase.

    The session phase machine is ``New -> Started -> Running -> Closed``:
    ``start()`` builds once from ``New`` and is an idempotent no-op on
    ``Started`` re-entry (Running/Closed re-entry raises); ``run()`` requires
    ``Started``; ``enqueue_path`` / ``trigger_discovery`` require ``Running``;
    ``shutdown()`` stays deliberately any-phase and idempotent (the SIGINT
    teardown ordering depends on it).

    The transition table itself is the Rust host's ``SessionPhase``
    (``strategy_host.rs``), read through ``degenbot._ffi.session_phase_next``;
    this exception is the Python surface of its refusals.
    """


class SessionPhase(Enum):
    """Cockpit session phase (the public signal is :class:`PhaseError`).

    Legality is the Rust host's ``SessionPhase`` table (``strategy_host.rs``),
    read through ``degenbot._ffi.session_phase_next``; the enum translates that
    verdict into :class:`PhaseError` and never authors the transition matrix.
    """

    NEW = "new"
    STARTED = "started"
    RUNNING = "running"
    CLOSED = "closed"

    def _next(self, operation: str) -> SessionPhase | None:
        """Return the host's verdict for ``operation`` (``None`` = refused).

        Args:
            operation: The host-side operation name (``start`` / ``run`` /
                ``query`` / ``shutdown``).

        Returns:
            The admitted next phase, or ``None`` when the host refuses the
            operation in this phase.

        """
        next_name = session_phase_next(self.value, operation)
        return None if next_name is None else SessionPhase(next_name)

    def on_start(self) -> SessionPhase:
        """Apply the operator startup move; the host admits it from New/Started.

        Returns:
            The admitted next phase.

        Raises:
            PhaseError: When the current phase refuses the start move.

        """
        next_phase = self._next("start")
        if next_phase is None:
            msg = f"start() in phase {self.value!r} - session can only start from New"
            raise PhaseError(msg)
        return next_phase

    def on_run(self) -> SessionPhase:
        """Apply the main-loop entry move; the host admits it from Started.

        Returns:
            The admitted next phase.

        Raises:
            PhaseError: When the current phase refuses the run move.

        """
        next_phase = self._next("run")
        if next_phase is None:
            msg = f"run() requires phase 'started' (session phase is {self.value!r})"
            raise PhaseError(msg)
        return next_phase

    def on_query(self, method: str) -> SessionPhase:
        """Apply the add-a-path/discovery gate; the host admits it while Running.

        Args:
            method: The operator method name (for the refusal message).

        Returns:
            The admitted next phase.

        Raises:
            PhaseError: When the current phase refuses the query.

        """
        next_phase = self._next("query")
        if next_phase is None:
            msg = f"{method}() needs 'running' (phase is {self.value!r})"
            raise PhaseError(msg)
        return next_phase

    def on_shutdown(self) -> SessionPhase:
        """Apply the teardown move; the host admits it from every phase.

        Returns:
            The admitted next phase.

        Raises:
            PhaseError: When the host table refuses the shutdown move.

        """
        next_phase = self._next("shutdown")
        if next_phase is None:  # pragma: no cover - the host table is total
            msg = f"shutdown() refused in phase {self.value!r}"
            raise PhaseError(msg)
        return next_phase


type _PathBuilder = Callable[..., Awaitable[None]]
type _Consumer = Callable[..., Coroutine[Any, Any, None]]
type _Scheduler = Callable[[Coroutine[Any, Any, None]], asyncio.Task[Any]]
type _PipelineFactory = Callable[[Session], Any]
type _FeeHistoryFetcher = Callable[..., Awaitable[bool]]


def _pick(injected: Any, production: Any) -> Any:
    """Answer the injected seam when one is set, else the production default.

    Args:
        injected: The injected value (``None`` = no injection).
        production: The production default.

    Returns:
        The seam the actor record carries.

    """
    return production if injected is None else injected


@dataclass(frozen=True)
class InjectedActors:
    """The frozen DI value a session resolves its actors from (``None`` = from cfg).

    This is the only injection surface: a seam left ``None`` is built from the
    typed config, a seam set is adopted verbatim. :meth:`Actors.resolve` turns
    it into the one :class:`Actors` record the session carries, so there is no
    parallel set of injected fields anywhere.
    """

    bot: Bot | None = None
    engine_registry: EngineRegistry | None = None
    async_w3: AsyncAlloyProvider | None = None
    path_builder: _PathBuilder | None = None
    consumer: _Consumer | None = None
    #: Factory for the session's sim/submit pipeline (``None`` = the production
    #: :class:`~degenbot.dispatch.BatchExecutor`); tests inject a stub to
    #: observe the pipeline seam without the Rust sim path.
    pipeline_factory: _PipelineFactory | None = None
    #: The head-tick ``eth_feeHistory`` leaf (Rust owns the decode +
    #: ``record_priority_fees``). Injected so a provider double can drive the
    #: reconcile path; the default is the production FFI leaf.
    fee_history_fetcher: _FeeHistoryFetcher | None = None
    #: Offline seam for the live activation gate: when set, the session's relay
    #: posture is this injected value (lifecycle tests pin a stub; production
    #: leaves it unset so the Rust readiness resolution owns the posture and the
    #: gate refuses an unsettled live boot).
    relay_posture: RelayPosture | None = None
    #: Forces the session's settlement-arm posture (None = derive from the
    #: readiness resolution). The run ritual reads this once at start():
    #: settlement off -> no path registration, no python-state trim, the active
    #: hosted arms (backrun drivers) enable around resume().
    settlement_arm: bool | None = None
    #: Factory for the activation-gate readiness resolution (None = the real
    #: ``validate_strategy_readiness``). A factory rather than a value so a test
    #: can express both the resolving gate and a raising refusal.
    readiness: Callable[[], StrategyReadinessView] | None = None
    #: Factory for the settlement broadcast endpoints (None = the real
    #: ``settlement_broadcast_endpoints``). The refusal rides the same
    #: ``ActivationGateRefused`` routing as the readiness gate.
    settlement_endpoints: Callable[[], list[str]] | None = None
    #: Offline seam for the engine-stop funnel (``shutdown()`` and the SIGINT
    #: handler both route through :meth:`Session._stop_engine`): when set, the
    #: funnel's stop is this injected callback; ``None`` (production) stops the
    #: real engine through the registry.
    stop_engine: Callable[[], None] | None = None
    #: The registration hand-off scheduler — the coroutine->task seam the run
    #: ritual consumes as DATA (the production default is
    #: ``asyncio.create_task``). There is no second registration mode to select:
    #: a deterministic test double drives the same hand-off inline.
    scheduler: _Scheduler = asyncio.create_task


@dataclass(frozen=True)
class Actors:
    """A session's resolved collaborators — production or injected, one shape.

    ``bot`` becomes ``None`` once the post-registration trim releases the
    Python-companion actor (the Rust engine keeps its own ``Bot`` ref); every
    other seam is fixed for the session's life.
    """

    bot: Bot | None
    engine_registry: EngineRegistry
    async_w3: AsyncAlloyProvider
    path_builder: _PathBuilder
    consumer: _Consumer
    scheduler: _Scheduler
    pipeline_factory: _PipelineFactory
    fee_history_fetcher: _FeeHistoryFetcher
    readiness: Callable[[], StrategyReadinessView]
    settlement_endpoints: Callable[[], list[str]]
    stop_engine: Callable[[], None] | None

    @classmethod
    async def resolve(cls, cfg: ArbitrageConfig, injected: InjectedActors) -> Actors:
        """Resolve a session's collaborators: injected values win, production fills.

        The actor trio is built from the config when not injected (the one
        asynchronous step); every other seam resolves through
        :meth:`from_injected`.

        Args:
            cfg: The session's typed config.
            injected: The DI value naming the seams to adopt verbatim.

        Returns:
            The resolved actor record.

        """
        trio = injected
        if trio.bot is None or trio.async_w3 is None:
            trio = replace(
                trio,
                bot=trio.bot if trio.bot is not None else build_bot(cfg),
                async_w3=(
                    trio.async_w3 if trio.async_w3 is not None else await build_async_w3(cfg)
                ),
            )
        if trio.engine_registry is None:
            assert trio.bot is not None, "resolve built the bot before the registry"
            trio = replace(trio, engine_registry=EngineRegistry(bot=trio.bot))
        return cls.from_injected(trio)

    @classmethod
    def from_injected(cls, injected: InjectedActors) -> Actors:
        """Assemble the one actor record over an injected trio (no production I/O).

        Args:
            injected: The DI value; its actor trio must be present.

        Returns:
            The resolved actor record.

        """
        bot = injected.bot
        engine_registry = injected.engine_registry
        async_w3 = injected.async_w3
        assert bot is not None, "the bot is built (or injected) before the record"
        assert engine_registry is not None, "the registry is built before the record"
        assert async_w3 is not None, "the provider is built (or injected) before the record"
        return cls(
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            path_builder=_pick(injected.path_builder, build_paths),
            consumer=_pick(injected.consumer, consume_result_batches),
            scheduler=injected.scheduler,
            pipeline_factory=_pick(injected.pipeline_factory, build_executor_for),
            fee_history_fetcher=_pick(injected.fee_history_fetcher, fetch_fee_history),
            readiness=_pick(injected.readiness, validate_strategy_readiness),
            settlement_endpoints=_pick(
                injected.settlement_endpoints, settlement_broadcast_endpoints
            ),
            stop_engine=injected.stop_engine,
        )


@dataclass
class Session:
    """One pump session: the owner of the cockpit's coordination state.

    Built in the ``New`` phase (config + the DI value only), then
    :meth:`start` fills in the built state exactly once. The ``None`` fields
    below are phase-dependent, not options: before ``start()`` there is no
    session state to hold, and ``actors`` is ``None`` exactly while the phase is
    ``New``.

    Write discipline: downstream modules (the run ritual, the block loop, the
    trim) read fields directly but mutate ONLY through the owner's mutators
    (``advance_block`` / ``attach_consumer_task`` / ``attach_registration`` /
    ``attach_registration_pipeline`` / ``drop_bot`` / ``build_pipeline``) —
    remote attribute pokes are forbidden.

    The mutable run-phase handles are the session's own lifecycle facts:
    ``result_consumer_task`` and ``registration_task``/``registration_context``
    are written by the run ritual through the mutators above, and
    ``sim_submit_pipeline``/``registration_pipeline`` attach once their
    producers land (after ``start()``, by design).
    """

    cfg: ArbitrageConfig
    injected: InjectedActors = field(default_factory=InjectedActors)
    install_sigint: bool = True
    phase: SessionPhase = SessionPhase.NEW
    #: The resolved collaborators (``None`` until ``start()`` resolves them; a
    #: boot refused after that leaves them resolved on a still-``New`` session,
    #: which is what lets ``shutdown()`` stop a half-started engine).
    actors: Actors | None = None
    #: The dispatch state (``None`` until ``start()``).
    dispatcher: Dispatcher | None = None
    #: The sim context; ``None`` for non-Alloy (test) providers, or unbuilt.
    sim_ctx: SimulateContext | None = None
    #: The session's block clock (advanced by the block loop's one mutation).
    current_block: int = 0
    #: The readiness view resolved once at the ``start()`` boundary.
    readiness: StrategyReadinessView | None = None
    #: The resolved settlement-arm disposition the run ritual reads.
    settlement_active: bool = False
    #: The session's relay posture (where signed bytes broadcast — see
    #: ``_relay_posture``); ``None`` for a backrun-only or dry-run boot.
    relay_posture: RelayPosture | None = None
    #: The per-session silent-veto smoke FSM (streak + throttle clock). Built
    #: with the session, so a streak can never leak into the next session.
    submission_smoke: SubmissionSmoke = field(default_factory=SubmissionSmoke)
    #: The one owner of the pump session's end-state (watch-set, verdict,
    #: teardown duties).
    session_watch: SessionWatch = field(default_factory=SessionWatch)
    #: The concurrent sim fan-out + single ordered submitter, attached by
    #: :meth:`build_pipeline` when the run ritual attaches the consumer.
    sim_submit_pipeline: BatchExecutor | None = None
    #: The operator add-a-path surface (attached by the run ritual when the real
    #: ``build_paths`` runs), kept reachable for the session's lifetime; ``None``
    #: for injected/fake runs.
    registration_pipeline: Any = None
    #: The registration-owned construction context (written by the run ritual
    #: for the real builder; ``None`` for injected builders).
    registration_context: ConstructionContext | None = None
    #: The result-consumer drain task (written by the run ritual at
    #: consumer-attach; read by the pump-finished watchdog and handed to the
    #: session watch). ``None`` before ``run()``.
    result_consumer_task: asyncio.Task[Any] | None = None
    #: The scheduled registration hand-off (the run ritual's scheduler output;
    #: the trim guard reads it to refuse a mid-climb outside trim). ``None`` for
    #: a backrun-only boot, which schedules nothing.
    registration_task: asyncio.Task[Any] | None = None
    _sigint_installed: bool = field(default=False, repr=False)
    _previous_sigint_handler: object = field(default=signal.SIG_DFL, repr=False)

    # ── Lifecycle ───────────────────────────────────────────────────
    async def start(self) -> Self:
        """Build the actors and run the engine's pre-resume ritual.

        Stops at ``Backfilled`` — BEFORE ``resume()``. Zero result batches emit
        during this window (the pump isn't running), so ``run()`` can attach the
        consumer in the gap before ``resume()`` without a stale-backlog window.
        Idempotent via the phase alone: re-entry once Started is a no-op;
        Running/Closed re-entry raises :class:`PhaseError`.

        Returns:
            The started session.

        Raises:
            RuntimeError: If the latest-block fetch fails at session start.

        """
        if self.phase is SessionPhase.STARTED:
            return self
        next_phase = self.phase.on_start()

        cfg = self.cfg

        # Arm the incident-diagnostic harnesses (tracemalloc diff thread,
        # /proc RSS CSV sampler, faulthandler repeat dump) from the typed
        # config — every cockpit-driven entrypoint gets them uniformly, and env
        # reads stay the loader's. Zero-config arms nothing.
        arm_diagnostics(cfg.diag)

        # ── The actor trio (injected or from cfg) ──
        actors = await Actors.resolve(cfg, self.injected)
        # The resolved record lands on the session BEFORE the boot can refuse
        # (the readiness gate, the posture mint): a half-started session still
        # reaches its engine, so shutdown() can stop a subscribed pump.
        self.actors = actors

        # ── Fetch current block (for the dispatcher + backfill comparison) ──
        latest_block = await actors.async_w3.get_block("latest")
        if latest_block is None:
            msg = "Failed to fetch the latest block at session start"
            raise RuntimeError(msg)
        current_block = latest_block["number"]

        # ── Coordination state ──
        dispatcher = Dispatcher.for_block(current_block)
        sim_ctx = build_sim_ctx(cfg, actors.async_w3, actors.engine_registry)

        # ── Engine pre-resume ritual (subscribe -> verify) ──
        # The snapshot->WS gap is closed automatically inside
        # `BlockPump::resume_from_subscribe` at resume. `start()` only
        # subscribes + sets up verify config; resume drives both the backfill
        # and the live loop. The DB snapshot is loaded eagerly at Bot
        # construction via `Bot::load_snapshot_from_db`; `start()` reads the
        # seed `S` from the core `BotState` (there are no snapshot kwargs).
        backfill_target = actors.engine_registry.start(
            cfg.node_http,
            cfg.node_ws,
            verify_state_view=EthereumMainnetUniswapV4.state_view.address,
        )
        if backfill_target > current_block:
            current_block = backfill_target
            dispatcher.advance_block(backfill_target)

        # The activation gate resolves ONCE here and caches the value; the run
        # ritual consumes that data rather than re-reading the verdict, while
        # the refusal rule stays with the Rust config validation.
        readiness = resolve_readiness(actors.readiness)
        # The settlement arm resolves before the ritual runs: the posture minter
        # and run()'s dispositions read this ONE resolved fact, so the session
        # can never carry a posture the arm disagrees with.
        settlement_active = self._resolve_settlement_arm(readiness)
        relay_posture = (
            self.injected.relay_posture
            if self.injected.relay_posture is not None
            else mint_relay_posture(
                settlement_active=settlement_active,
                live=not cfg.dry_run,
                endpoints=actors.settlement_endpoints,
            )
        )

        self.dispatcher = dispatcher
        self.sim_ctx = sim_ctx
        self.current_block = current_block
        self.readiness = readiness
        self.settlement_active = settlement_active
        self.relay_posture = relay_posture
        self._install_sigint_handler()
        self.phase = next_phase
        return self

    async def run(self) -> SessionEndVerdict:
        """Run the cockpit main loop until the consumer task ends.

        Requires the ``Started`` phase — the session phase machine
        (:class:`PhaseError`, delegating to the Rust host's ``SessionPhase``
        table) owns the lifecycle gate. The startup ordering itself is the run
        ritual's (:mod:`~degenbot.runner._run_ritual`): this method is the thin
        phase-gated drive over that machine.

        Returns:
            The session's end verdict (the session watch's ranking over how the
            run ended).

        """
        self.phase = self.phase.on_run()
        return await RunRitual(self).run()

    async def shutdown(self) -> None:
        """Signal the Rust core to stop the pump (best-effort).

        Safe to call at any point in the lifecycle — before ``start()`` finished
        (there are no actors yet), after ``run()`` exited, or from a
        ``SIGINT``/``KeyboardInterrupt`` handler. Mirrors the Rust ``stop()``
        contract: idempotent, sets the shutdown flag + aborts the pump task so
        the WS stream's ``combined.next().await`` unblocks immediately (60s
        cold-shutdown otherwise). Any exception is swallowed and logged so a
        partial-startup teardown can't mask the original in-flight exception.

        This is the one place that closes the Rust core's pump — the
        ``KeyboardInterrupt``-exits-slowly bug was the pump task (spawned on the
        shared tokio runtime, decoupled from the asyncio loop) blocking on a
        silent WS subscription, which ``asyncio.run``'s teardown did not reach
        until the OS closed the socket.
        """
        # Closed from ANY phase: teardown (SIGINT, partial startup, post-run)
        # may reach here at any point; idempotent by design, and the Rust host's
        # SessionPhase table owns the verdict.
        self.phase = self.phase.on_shutdown()
        self._stop_engine()
        # ADR-043 §6: flush the telemetry providers BEFORE this session — and
        # the Rust core's tokio runtime behind it — is torn down. An OTLP batch
        # flushed after runtime teardown exports nothing, so the tail of a run
        # would be silently lost. None-safe when telemetry is off.
        try:
            from degenbot.telemetry import flush_telemetry as _flush_telemetry

            _flush_telemetry()
        except Exception as exc:
            bot_logger.debug(f"[shutdown] telemetry flush failed: {exc!r}")

    async def close(self) -> None:
        """Tear the session down: stop the pump, restore SIGINT, reap the watch.

        The ordering rationale: the pump must be stopped BEFORE the consumer
        task is cancelled. The consumer awaits ``engine.__anext__()`` which
        blocks on the pump's result channel; cancelling the consumer first
        leaves the pump's WS task running on the shared tokio runtime, blocking
        process exit until the WS subscription closes itself (up to 60s on a
        silent stream). Stopping the pump first closes the channels -> the
        consumer's next ``__anext__`` raises ``StopAsyncIteration`` -> the
        consumer task ends cleanly.

        The seam between the two halves is permanent. Stopping the pump is
        engine-owned — one latch/abort/join contract shared by every driver —
        while cancelling an asyncio task is host-side by nature, so neither half
        can absorb the other and their ordering lives at this call site.
        """
        await self.shutdown()
        self._restore_sigint_handler()
        # The consumer-cancel duty lives on the session watch — its teardown()
        # re-checks the registration + watchdog drains (idempotent no-ops once
        # run() unwound) and cancels the consumer AFTER the pump was stopped
        # above.
        await self.session_watch.teardown()

    async def __aenter__(self) -> Self:
        """Hand the started session to the ``async with`` block.

        Returns:
            The started session.

        """
        await self.start()
        return self

    async def __aexit__(self, *exc: object) -> None:
        """Tear the session down; never suppresses the in-flight exception.

        Args:
            *exc: The in-flight exception triple (unused).

        """
        await self.close()

    # ── Operator add-a-path surface ─────────────────────────────────
    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path at any time (the operator surface).

        Delegates to the session's live :class:`PathRegistrationPipeline`
        (created by the run ritual); ``path_steps`` + optional ``directions``
        are the same shapes as :meth:`PathRegistrationPipeline.enqueue_path`.

        Args:
            path_steps: The path's steps (the pipeline's enqueue shape).
            directions: Per-step swap directions (``None`` = resolve them).

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders
                have no construction surface, or ``run()`` has not run).

        """
        self.phase = self.phase.on_query("enqueue_path")
        if self.registration_pipeline is None:
            msg = "no live registration pipeline; add-path unavailable (injected/fake run)"
            raise RuntimeError(msg)
        await self.registration_pipeline.enqueue_path(path_steps, directions=directions)

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (on-demand trigger).

        Delegates to the session's live pipeline.

        Args:
            bound: The maximum number of paths the sweep processes.

        Returns:
            The number of paths processed by the sweep.

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders,
                or ``run()`` has not run).

        """
        self.phase = self.phase.on_query("trigger_discovery")
        if self.registration_pipeline is None:
            msg = "no live registration pipeline; on-demand discovery unavailable"
            raise RuntimeError(msg)
        return cast(
            "int",
            await self.registration_pipeline.trigger_discovery(bound=bound),
        )

    # ── Run-phase duties ────────────────────────────────────────────
    async def build_pipeline(self) -> BatchExecutor:
        """Build (through the session's factory) and attach the sim/submit pipeline.

        The factory may be async (the production construction dials the relay
        providers + seeds the nonce lane) or sync (a test double observes the
        attach without the Rust sim seam).

        Returns:
            The attached pipeline.

        """
        actors = self.actors
        assert actors is not None, "the pipeline is built by a started session"
        built = actors.pipeline_factory(self)
        pipeline = await built if inspect.isawaitable(built) else built
        self.sim_submit_pipeline = cast("BatchExecutor", pipeline)
        return self.sim_submit_pipeline

    def block_loop(self) -> BlockLoop:
        """Assemble the block loop's collaborator record over the built state.

        Returns:
            The block loop's collaborators (clock, provider, pipeline, engine,
            fee-history leaf, display policy, smoke FSM, block-clock mutator).

        Raises:
            RuntimeError: If the session is not started or has no attached
                sim/submit pipeline.

        """
        actors = self.actors
        dispatcher = self.dispatcher
        pipeline = self.sim_submit_pipeline
        if actors is None or dispatcher is None or pipeline is None:
            msg = "the block loop needs a started session with an attached sim/submit pipeline"
            raise RuntimeError(msg)
        return BlockLoop(
            dispatcher=dispatcher,
            engine=actors.engine_registry.engine,
            provider=actors.async_w3,
            pipeline=pipeline,
            fee_history=actors.fee_history_fetcher,
            policy=DisplayPolicy.from_cfg(self.cfg),
            smoke=self.submission_smoke,
            advance_block=self.advance_block,
        )

    def trim_python_state(self, *, close_read_tx: bool = True) -> None:
        """Trim redundant Python state once registration is done.

        Refuses while a background registration is live and still climbing: only
        the registration task may trim its own state (its completion hand-off,
        or the cancel branch). Releases the held snapshot read tx, then drops
        the Python-side caches and nulls the session's bot ref so the hot loop
        isn't pinning Python pool objects.

        Args:
            close_read_tx: on the healthy path (``build_paths`` completed) the
                ``Arc<SnapshotDb>`` canary fires and the read tx is committed to
                reclaim WAL space. On the mid-registration cancel/teardown
                branch it is ``False`` — build-worker ``Arc<SnapshotDb>`` clones
                may still be live, so the canary would false-positive; the tx is
                instead dropped with ``Bot`` at process teardown. Callers must
                keep the canary active whenever registration actually finished.

        Raises:
            RuntimeError: If a background registration is still climbing (only
                the registration task may trim its own state).

        """
        # The shared tracker/pool/token registries this trim clears are also
        # held, mutably, by a still-climbing registration's ConstructionContext.
        # Only the registration task may trim while it is live (its own
        # completion hand-off, or its cancel branch); an outside caller is
        # mid-climb and would clobber them.
        registration_task = self.registration_task
        if registration_task is not None and not registration_task.done():
            try:
                current = asyncio.current_task()
            except RuntimeError:  # no running loop: not the registration task
                current = None
            if current is not registration_task:
                msg = (
                    "refusing to trim Python state while background "
                    "registration is still climbing: the registration's "
                    "ConstructionContext holds the same tracker/pool/token "
                    "registries this trim clears; only the registration task "
                    "may trim its own state"
                )
                raise RuntimeError(msg)
        actors = self.actors
        assert actors is not None, "the trim runs on a started session"
        registry = actors.engine_registry
        bot = actors.bot
        if bot is None:
            return
        # Release the held snapshot read transaction: `load_snapshot_from_db`
        # opened a deferred read tx so every `assemble_*_tick_map` Db-arm read
        # during `build_paths` shared one frozen DB snapshot. Pool registration
        # is done — commit the tx to release the WAL snapshot so the updater's
        # checkpoint can reclaim `-wal` space for the hot loop. No-op for the
        # cold-start path (no DB). `getattr` so test fakes (`_FakeBot`) without
        # a real `_py_bot` skip. Skipped entirely on the cancel/teardown branch:
        # in-flight executor `assemble_*` clones would trip the canary, and the
        # WAL is moot once the process is exiting.
        py_bot = getattr(bot, "_py_bot", None)
        if py_bot is not None and close_read_tx:
            py_bot.close_snapshot_tx()

        # Trim redundant Python state — Rust engine owns canonical pool state.
        bot.release_python_state()
        self.drop_bot()  # drop the only Python ref; engine keeps its own Bot ref
        gc.collect()

        bot_logger.info(
            f"[startup] State trimmed — "
            f"{registry.engine.reserve_pair_pool_count()} reserve-pair, "
            f"{registry.engine.v3_pool_count()} V3, "
            f"{registry.engine.v4_pool_count()} V4 pools retained in "
            f"Rust engine; {registry.engine.path_count()} paths registered. "
            f"Entering main loop.",
        )

    async def pump_finished_watchdog(self) -> None:
        """Await the core session-end detection fact, then cancel the consumer.

        The hotpath timed exit (``HOTPATH_SHUTDOWN_MS``) makes the pump return
        normally after writing its report. Without this watcher the session's
        idle consumer task keeps the process alive forever on a dead engine —
        ``run_bot.sh`` then reports "running" while nothing progresses (the
        post-unwind wedge: gil-probe idle for minutes after the pump exited).
        The completion surface also fires on a pump panic (the pump task owns
        the channel's only sender; unwinding drops it), so this doubles as a
        panic fail-safe.

        The await is the real Rust-backed detection fact for every engine —
        there is no injected-engine/no-surface bypass: test doubles satisfy the
        same awaitable contract (a fake pump that never finishes parks here
        forever, which is exactly the pre-finish consumer shape). The driver
        only reads the core fact and cancels the consumer; the verdict ranking
        stays with the session watch.

        Returns once the pump finished; the consumer was cancelled here, so the
        session watch observes the pump end as ``WatchdogTripped``.
        """
        actors = self.actors
        assert actors is not None, "the watchdog runs on a started session"
        cause = await actors.engine_registry.engine.session_end_future()
        bot_logger.warning(
            f"[shutdown] core session-end fact {cause} outside stop() — "
            "cancelling the consumer for a graceful teardown"
        )
        main_task = self.result_consumer_task
        if main_task is not None and not main_task.done():
            main_task.cancel()

    # ── Mutators (the one write discipline) ─────────────────────────
    def advance_block(self, block_number: int) -> None:
        """Advance the session's block clock (the block loop's one mutation).

        Args:
            block_number: The head block the tick reported.

        """
        self.current_block = block_number

    def attach_consumer_task(self, task: asyncio.Task[Any]) -> None:
        """Attach the result-consumer drain task (written by the run ritual).

        Args:
            task: The consumer task the session watch and watchdog read.

        """
        self.result_consumer_task = task

    def attach_registration(
        self,
        task: asyncio.Task[Any],
        context: ConstructionContext | None,
    ) -> None:
        """Attach the scheduled registration hand-off and its construction context.

        Args:
            task: The scheduled ``build_paths`` hand-off.
            context: The registration-owned construction context (``None`` for
                an injected builder, which has no construction surface).

        """
        self.registration_task = task
        self.registration_context = context

    def attach_registration_pipeline(self, pipeline: Any) -> None:
        """Attach the operator add-a-path surface (built by the run ritual).

        Args:
            pipeline: The live :class:`PathRegistrationPipeline`.

        """
        self.registration_pipeline = pipeline

    def drop_bot(self) -> None:
        """Release the Python-companion bot ref (the post-registration trim).

        The Rust engine keeps its own ``Bot`` ref; dropping the Python handle
        lets the pool/token objects the trim released be collected.
        """
        actors = self.actors
        assert actors is not None, "the trim runs on a started session"
        self.actors = replace(actors, bot=None)

    # ── Test seam ───────────────────────────────────────────────────
    @classmethod
    def for_test(  # ruff:ignore[too-many-arguments] -- a construction seam is wide by nature: it replaces a per-test DI record plus direct field pokes
        cls,
        cfg: ArbitrageConfig,
        *,
        actors: Actors,
        dispatcher: Dispatcher,
        current_block: int = 0,
        sim_ctx: SimulateContext | None = None,
        settlement_active: bool = True,
        readiness: StrategyReadinessView | None = None,
        relay_posture: RelayPosture | None = None,
        session_watch: SessionWatch | None = None,
        install_sigint: bool = False,
    ) -> Session:
        """Build a started session directly over injected collaborators.

        The one construction seam for the cockpit tests: it resolves the state
        ``start()`` would build (actors, dispatcher, block clock, settlement
        arm, posture) without the boot I/O, so a test supplies only the
        collaborators its facet exercises instead of a 13-field DI record.

        Args:
            cfg: The session's typed config.
            actors: The resolved collaborators (the standard fakes, or the real
                production builders).
            dispatcher: The session's dispatch state.
            current_block: The block clock the session starts on.
            sim_ctx: The sim context (``None`` for non-Alloy providers).
            settlement_active: The resolved settlement-arm disposition.
            readiness: The resolved readiness view (the run ritual reads it for
                the hosted-arm facet set).
            relay_posture: The relay posture to broadcast through (``None`` =
                no-relay posture).
            session_watch: The end-state owner to use (``None`` = a fresh one).
            install_sigint: Whether to bind the SIGINT handler.

        Returns:
            A session in the ``Started`` phase.

        """
        session = cls(
            cfg=cfg,
            phase=SessionPhase.STARTED,
            actors=actors,
            dispatcher=dispatcher,
            current_block=current_block,
            sim_ctx=sim_ctx,
            settlement_active=settlement_active,
            readiness=readiness,
            relay_posture=relay_posture,
            install_sigint=install_sigint,
        )
        if session_watch is not None:
            session.session_watch = session_watch
        session._install_sigint_handler()
        return session

    # ── Settlement arm + engine stop + SIGINT ───────────────────────
    def _resolve_settlement_arm(self, readiness: StrategyReadinessView) -> bool:
        """Resolve the settlement arm once, before the run ritual reads it.

        The injected flag (test/probe DI) wins; otherwise the readiness view the
        posture gate already walked owns the answer — the same facet activation
        the gate used, so the arm never disagrees with the fleet. This is the
        one resolved fact the posture minter and run()'s dispositions consume.

        Args:
            readiness: The readiness view the gate resolved for this boot.

        Returns:
            The resolved settlement-arm disposition.

        """
        if self.injected.settlement_arm is not None:
            return self.injected.settlement_arm
        return bool(readiness.settlement_active)

    def _stop_engine(self) -> None:
        """Stop the engine — the ONE entrypoint (``shutdown()`` + SIGINT funnel here).

        Mirrors the Rust ``stop()`` contract: idempotent, sets the shutdown
        flag, and aborts the pump task. Best-effort — a torn-down engine during
        a partial startup must not mask the in-flight exception. An injected
        ``stop_engine`` actor (tests) replaces the registry stop; ``None``
        (production) stops the real engine.
        """
        actors = self.actors
        if actors is None:
            return
        if actors.stop_engine is not None:
            actors.stop_engine()
            return
        try:
            actors.engine_registry.engine.stop()
        except Exception as exc:
            bot_logger.warning(f"[shutdown] engine.stop() failed: {exc!r}")

    def _install_sigint_handler(self) -> None:
        """Bind a SIGINT handler that stops the Rust pump *immediately*.

        The ``close()`` -> ``shutdown()`` -> ``engine.stop()`` path only fires
        once the awaited coroutine unwinds — and during ``build_paths`` the main
        thread is blocked inside the synchronous ``find_paths`` graph prep / the
        Rust ``find_paths_rust`` DFS. Python's default SIGINT -> raise
        ``KeyboardInterrupt`` mechanism is *deferred* until that section yields
        control to the eval loop, so the first Ctrl-C appeared to be swallowed:
        the pump (on the shared tokio runtime, a separate thread) kept running,
        the operator pressed Ctrl-C again, and only when ``find_paths`` finally
        returned did the deferred exception unwind to ``close()`` and stop the
        pump.

        Installing this handler closes the gap: the moment SIGINT arrives,
        ``engine.stop()`` runs (it just sets the shutdown flag + aborts the pump
        task — cheap, GIL-only, idempotent) regardless of what the main thread
        is doing. The Rust ``find_paths_rust`` DFS releases the GIL via
        ``py.detach()``, so the handler *can* run even mid-DFS. We then re-raise
        so the normal ``KeyboardInterrupt`` unwind proceeds to ``close()``
        (which runs ``shutdown()`` again — a no-op — for the consumer
        cancellation).

        Idempotent: if already installed (or if ``signal`` can't bind — e.g. a
        non-main thread), it's a no-op so the call site in ``start()`` is safe
        to re-enter.
        """
        actors = self.actors
        if self._sigint_installed or not self.install_sigint or actors is None:
            return
        try:
            self._previous_sigint_handler = signal.getsignal(signal.SIGINT)
        except ValueError:
            # `signal.signal` only works on the main thread; if start() is ever
            # driven off-thread there is nothing to bind — rely on close()'s
            # shutdown() alone.
            return

        signal.signal(signal.SIGINT, self._handle_sigint)
        self._sigint_installed = True

    def _handle_sigint(self, _signum: int, _frame: object) -> None:
        """Stop the pump through the one funnel the instant SIGINT arrives.

        Bound (not a closure) so the funnel is a named, testable seam. Fires
        even while the main thread is blocked in ``find_paths`` (the Rust DFS
        releases the GIL), then re-raises ``KeyboardInterrupt`` so the awaiting
        coroutine unwinds through ``close()`` -> ``shutdown()`` (idempotent).

        Args:
            _signum: The signal number (unused).
            _frame: The interrupted frame (unused).

        Raises:
            KeyboardInterrupt: Always — after the engine stop, so the awaiting
                coroutine unwinds through the teardown.

        """
        self._stop_engine()
        raise KeyboardInterrupt

    def _restore_sigint_handler(self) -> None:
        """Restore the SIGINT handler ``start()`` replaced (a no-op when unbound)."""
        if not self._sigint_installed:
            return
        with contextlib.suppress(ValueError, TypeError):
            signal.signal(signal.SIGINT, cast("Any", self._previous_sigint_handler))
        self._sigint_installed = False
