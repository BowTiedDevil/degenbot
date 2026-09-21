"""Settlement-arbitrage runtime driver facade (``BotRunner``).

``BotRunner`` is the
Python-companion cockpit over the Rust-owned engine: it owns the config and
the lifecycle, while the coordination state itself (the three actors
``bot``/``engine_registry``/``async_w3`` + the ``Dispatcher`` + the block
clock) is owned by the ONE ``_SessionState`` built in ``start()``.
It is the one place that enforces the phase ordering the engine's state
machine requires.

    start():  subscribe -> stream snapshots -> backfill -> verify config
              (``EngineRegistry.start``, stops at Backfilled, pre-resume)
    run():    attach consumer -> ``resume()`` -> registration -> main loop

The driver is ``stays-python`` (asyncio loop, SIGINT, deployment policy): it
controls the Rust engine but owns no pool state (ADR-003: ``Bot`` is the
single state owner; ADR-006: ``Bot`` is the per-chain orchestrator).
**Sequencing supersedure (ADR-050, 2026-09-14):** the start/run sequencing
described here is Rust-owned in ``EngineDriver``; only asyncio/SIGINT/
deployment policy stays ``stays-python`` — see
``docs/architecture/rust-settlement-bot-parity.md``. It
delegates path registration to :mod:`~degenbot.runner.build_paths` and the
main loop to :mod:`~degenbot.runner.consume`.

Testability seams (mirrors ``EngineRegistry``'s ``engine=`` seam): ``bot``,
``engine_registry``, ``async_w3``, ``snapshots``, ``path_builder``, and
``consumer`` are injectable.
"""

from __future__ import annotations

import asyncio
import contextlib
import gc
import signal
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Self, cast

from degenbot import Bot
from degenbot.arbitrage import session_phase_next
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
)
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.dispatch import Dispatcher, SimulateContext
from degenbot.logging import logger as bot_logger
from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.runner._consume import consume_result_batches
from degenbot.runner._dispatch import SubmissionSmoke, _load_executor_runtime_bytecode
from degenbot.runner._driver_constants import (
    ETH_MAINNET_ALLOWED_TOKENS,
    MULTICALL3_ADDRESS,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
from degenbot.runner._relay_posture import RelayPosture
from degenbot.runner._session_watch import SessionEndVerdict, SessionWatch
from degenbot.runner._sim_submit_pipeline import SimSubmitPipeline
from degenbot.runner.build_paths import (
    BuildPathsOptions,
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
)
from degenbot.runner.config import ArbitrageConfig
from degenbot.runner.diag import arm_diagnostics
from degenbot.uniswap.deployments import EthereumMainnetUniswapV4
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

# _make_arbitrage_config


def _make_arbitrage_config(node_http: str) -> DegenbotConfig:
    """Build a single-chain DegenbotConfig for the arbitrage session (ADR-006).

    The chain identity is Ethereum mainnet (1); the RPC is the caller's
    ``node_http`` — the cascade-resolved endpoint from
    :func:`degenbot.config.resolve_rpc_uris` (CLI > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_1`` > config.toml ``rpc[1]``). When
    config.toml was the winning source, ``node_http`` already
    equals ``rpc[1]``, so the injection here is consistent rather than a bypass.
    The Bot enforces the connected RPC's ``eth_chainId`` matches at construction.

    The database path is read from the existing user config at the standard
    config file (``$XDG_CONFIG_HOME``/``$HOME/.config`` ``degenbot/config.toml``,
    so locally-configured DB paths are honored) and falls back to the XDG
    state-home default if no config exists.
    """
    from degenbot.config import CONFIG_FILE, load_config_from_file

    if CONFIG_FILE.exists():
        base = load_config_from_file(CONFIG_FILE)
        # Override the RPC with the env-derived endpoint while keeping
        # the database path (and any other settings) from the config file.
        return DegenbotConfig(
            database=base.database,
            rpc={1: cast("Any", node_http)},
            default_chain_id=1,
        )

    from degenbot.config import DB_PATH

    return DegenbotConfig(
        database=DatabaseSettings(path=DB_PATH),
        rpc={1: cast("Any", node_http)},
        default_chain_id=1,
    )


# ──────────────────────────────────────────────────────────────────
# Direction resolver
# ──────────────────────────────────────────────────────────────────


# ============ class BotRunner ============


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


class _Phase(Enum):
    """Cockpit session phase (private; the public signal is :class:`PhaseError`).

    Legality is the Rust host's ``SessionPhase`` table (``strategy_host.rs``),
    read through ``degenbot._ffi.session_phase_next``; the enum translates that
    verdict into :class:`PhaseError` and never authors the transition matrix.
    """

    NEW = "new"
    STARTED = "started"
    RUNNING = "running"
    CLOSED = "closed"

    def _next(self, operation: str) -> _Phase | None:
        """The host's verdict for ``operation`` (``None`` = refused)."""
        next_name = session_phase_next(self.value, operation)
        return None if next_name is None else _Phase(next_name)

    def on_start(self) -> _Phase:
        """The operator startup move; the host admits it from New/Started."""
        next_phase = self._next("start")
        if next_phase is None:
            msg = f"start() in phase {self.value!r} - session can only start from New"
            raise PhaseError(msg)
        return next_phase

    def on_run(self) -> _Phase:
        """The main-loop entry move; the host admits it from Started."""
        next_phase = self._next("run")
        if next_phase is None:
            msg = f"run() requires phase 'started' (session phase is {self.value!r})"
            raise PhaseError(msg)
        return next_phase

    def on_query(self, method: str) -> _Phase:
        """The add-a-path/discovery gate; the host admits it while Running."""
        next_phase = self._next("query")
        if next_phase is None:
            msg = f"{method}() needs 'running' (phase is {self.value!r})"
            raise PhaseError(msg)
        return next_phase

    def on_shutdown(self) -> _Phase:
        """The teardown move; the host admits it from every phase."""
        next_phase = self._next("shutdown")
        if next_phase is None:  # pragma: no cover - the host table is total
            msg = f"shutdown() refused in phase {self.value!r}"
            raise PhaseError(msg)
        return next_phase


@dataclass
class _SessionState:
    """Cockpit session state (CONTEXT.md term: *session state*).

    The single owner of one pump session's coordination state, built REAL in
    ``start()`` — no pre-session option cluster survives on the runner (its
    same-named attributes are a facade over this object once it exists). The
    block loop (``consume``) and the dispatch leaf
    (``dispatch``) both read the same owner instead of the session travelling
    as a parameter bag.

    Write discipline: downstream modules (the block loop, the dispatch leaf,
    the sim/submit pipeline) read fields directly but mutate ONLY through the
    owner's mutator methods (``advance_block`` / ``attach_pipeline`` /
    ``attach_registration_pipeline``) — remote attribute pokes are forbidden.
    The ``None`` fields are domain options, not phase options: ``sim_ctx`` is
    ``None`` only for non-Alloy (test) providers, ``bot`` becomes ``None``
    when the post-registration trim drops it, and the two pipelines attach
    lazily through their mutators (their producers legitimately land after
    ``start()``).

    Mutable pieces (``current_block``) advance on the owner.
    """

    engine_registry: EngineRegistry
    async_w3: AsyncAlloyProvider
    sim_ctx: SimulateContext | None
    dispatcher: Dispatcher
    cfg: ArbitrageConfig
    current_block: int
    #: The Python-companion bot actor — dropped (``None``) by the
    #: post-registration trim; the engine keeps its own Bot ref.
    bot: Bot | None = None
    #: The concurrent sim fan-out + single ordered submitter (attached at
    #: consumer start via :meth:`attach_pipeline`); ``None`` runs the
    #: serial leaf.
    sim_submit_pipeline: SimSubmitPipeline | None = None
    #: The operator add-a-path surface (attached in run() via
    #: :meth:`attach_registration_pipeline` when the real build_paths runs),
    #: kept reachable for the session's lifetime; ``None`` for injected/fake
    #: runs.
    registration_pipeline: Any = None
    #: The session's relay posture (where signed bytes broadcast — see
    #: ``_relay_posture``). Resolved once here from the typed config; ``None``
    #: for injected sessions constructed directly, which the submit seam then
    #: treats as no-relay posture. Nonce issuance is the Rust authority's.
    relay_posture: RelayPosture | None = None
    #: The per-session silent-veto smoke FSM (streak + throttle clock). Built
    #: with the session, so a streak can never leak into the next session.
    submission_smoke: SubmissionSmoke = field(default_factory=SubmissionSmoke)

    def advance_block(self, block_number: int) -> None:
        """Advance the session's block clock (the consumer's one mutation)."""
        self.current_block = block_number

    def attach_pipeline(self, pipeline: SimSubmitPipeline) -> None:
        """Attach the lazily-built sim/submit pipeline."""
        self.sim_submit_pipeline = pipeline

    def attach_registration_pipeline(self, pipeline: Any) -> None:
        """Attach the operator add-a-path surface (built in run())."""
        self.registration_pipeline = pipeline


@dataclass(frozen=True)
class InjectedActors:
    """Test/DI actor overrides for :class:`BotRunner` (``None`` = build from cfg)."""

    bot: Bot | None = None
    engine_registry: EngineRegistry | None = None
    async_w3: AsyncAlloyProvider | None = None
    snapshots: tuple[Any, Any, Any, Any] | None = None
    path_builder: Any = None
    consumer: Any = None
    #: Offline seam for the live activation gate: when set, the session's
    #: relay posture is this injected value (lifecycle tests pin a stub;
    #: production leaves it unset so the Rust readiness resolution owns the
    #: posture and the gate refuses an unsettled live boot).
    relay_posture: RelayPosture | None = None


class BotRunner:
    """Orchestrator that collapses the settlement-arbitrage startup ritual behind one facade.

    Owns the config and the lifecycle; the coordination state itself (the
    three actors ``bot``/``engine_registry``/``async_w3``, the ``Dispatcher``,
    the block clock, the sim context, the pipelines) lives on ONE
    :class:`_SessionState` built in ``start()`` — the runner's
    same-named attributes are a facade over that owner, not mirrors. The
    runner is the ONE place that enforces the phase ordering the engine's
    state machine requires:

        start():  subscribe → stream snapshots → backfill → verify config
                  (``EngineRegistry.start``, stops at Backfilled, pre-resume)
        run():    attach consumer → ``resume()`` → [spawn background
                  registration → trim on completion (production) | await
                  build_paths → trim (injected)] → main loop; a cross-task
                  fail-fast channel surfaces a fatal registration error.

    Usage (production)::

        cfg = ArbitrageConfig.from_env(
            dotenv_values("examples/mainnet.env"), live=not dry_run, permutation=args.permutation
        )
        async with BotRunner(cfg) as session:
            await session.run()

    In production ``run()`` spawns discovery+registration as a
    background task and enters the main loop immediately; the state-trim runs
    on registration completion (in the background task), not on the main-loop
    entry path, so it cannot clobber the shared registries mid-flight. A fatal
    verification error still crashes loudly through the cross-task channel.
    The hot loop keeps only ``engine_registry`` + ``async_w3`` + dispatcher
    once trimmed — the Python pool/token caches are scaffolding once the Rust
    engine owns canonical state.

    Testability seams (mirrors ``EngineRegistry``'s ``engine=`` seam): ``bot``,
    ``engine_registry``, ``async_w3``, ``snapshots``, ``path_builder``, and
    ``consumer`` are injectable. When injected, ``start()``/``run()``
    orchestrate the fakes and the phase ordering is verifiable offline; when
    ``None`` (production), the actors are built from ``cfg`` and the real
    module functions are called.
    """

    def __init__(
        self,
        cfg: ArbitrageConfig,
        *,
        actors: InjectedActors | None = None,
        install_sigint: bool = True,
        background_registration: bool | None = None,
    ) -> None:
        """Store config + injectable test actors; the real actors are built in ``start()``.

        ``actors`` bundles the test actor seams (``bot``/``engine_registry``/
        ``async_w3``/``snapshots``/``path_builder``/``consumer``).

        ``background_registration`` (default ``None`` → auto) controls the
        background-registration seam: when ``True`` ``run()`` spawns discovery+registration as a
        background task (decoupled from the main loop, cross-task fail-fast);
        when ``False`` it awaits the path builder synchronously + trims
        immediately (legacy orchestration, used by tests). ``None`` auto-selects
        ``False`` for injected ``path_builder`` (tests) and ``True`` for the real
        ``build_paths`` (production).
        """
        # Strategy admission is host-owned (ADR-057): the runner carries no
        # Python-side arm gate. The host boot registers each configured facet
        # and `enable_strategy` surfaces the typed refusal
        # (`UnknownStrategyError` / `UnconfiguredStrategyError`), so there is
        # one admission authority and no env check that can disagree with the
        # host's configured-ness.
        self.cfg = cfg
        injected = actors if actors is not None else InjectedActors()
        self._injected_bot = injected.bot
        self._injected_engine_registry = injected.engine_registry
        self._injected_async_w3 = injected.async_w3
        self._injected_snapshots = injected.snapshots
        self._path_builder = injected.path_builder
        self._consumer = injected.consumer
        self._injected_relay_posture = injected.relay_posture
        self._background_registration: bool | None = background_registration
        # The registration-owned construction context (built in run() for
        # the real build_paths; None for injected builders and until run()).
        self._registration_context: ConstructionContext | None = None
        # The background registration task (production + explicit
        # ``background_registration=True``), awaited for fail-fast in step 5.
        # (The session WATCH owns the task set — the runner keeps these
        # handles only to hand them over + drive the watchdog.)
        self._registration_task: asyncio.Task | None = None
        # Snapshots for the registration pass (nulled by the trim).
        self.v3_snapshot: Any = None
        self.v4_snapshot: Any = None
        # The phase machine is the ONLY lifecycle state (no ``_started`` bool
        # — Started re-entry is the no-op; Running/Closed re-entry is the
        # phase error).
        self._phase: _Phase = _Phase.NEW
        # THE session (built in start()): the one owner of the coordination
        # state — actors, dispatcher, block clock, sim context, pipelines.
        # ``None`` only before start() (there is no session yet; shutdown()'s
        # any-phase contract and the pre-start injection seams rely on that).
        self._session: _SessionState | None = None
        # Created in run():
        self._result_consumer_task: asyncio.Task | None = None
        # The one owner of the pump session's end-state — the
        # watch-set ({consumer} + optional {registration, watchdog}), the
        # SessionEndVerdict ranking, and the cancel/teardown duties for the
        # run()-finally and __aexit__ sites. Attached in run(); awaited
        # there; torn down from __aexit__.
        self._session_watch = SessionWatch()
        # SIGINT handler installed by `start()`, restored by `__aexit__`.
        # Stores the previous handler so teardown restores it (the default
        # SIGINT → KeyboardInterrupt machinery) rather than leaving a
        # process-wide handler bound after the session ends.
        self._previous_sigint_handler: object = signal.SIG_DFL
        self._sigint_installed = False
        # Production (main()) installs the SIGINT→engine.stop() handler so a
        # Ctrl-C during the synchronous find_paths section stops the pump
        # immediately. Tests pass install_sigint=False to avoid binding a
        # process-global handler (signal.signal pollutes across tests).
        self._install_sigint = install_sigint

    # ── Session facade ───────────────────────────────────────────────
    # The _SessionState built in start() is the one STORAGE for the actors,
    # the dispatcher, the block clock, and the sim context. These properties
    # are the runner's facade over it — after start() they read/write the
    # session (no runner-side mirrors); before start() there is no session,
    # so they read/write the injection seams (a pre-start write IS an
    # injection: start() adopts it when building the session).

    @property
    def bot(self) -> Bot | None:
        """The session's Python-companion bot (``None`` once the trim dropped it).

        Before ``start()``: the injected seam (a write is an injection)."""
        return self._session.bot if self._session is not None else self._injected_bot

    @bot.setter
    def bot(self, bot: Bot | None) -> None:
        if self._session is not None:
            self._session.bot = bot
        else:
            self._injected_bot = bot

    @property
    def engine_registry(self) -> EngineRegistry | None:
        """The session's engine registry. Before ``start()``: the injected seam."""
        return (
            self._session.engine_registry
            if self._session is not None
            else self._injected_engine_registry
        )

    @engine_registry.setter
    def engine_registry(self, registry: EngineRegistry) -> None:
        if self._session is not None:
            self._session.engine_registry = registry
        else:
            self._injected_engine_registry = registry

    @property
    def async_w3(self) -> AsyncAlloyProvider | None:
        """The session's dispatch-path provider. Before ``start()``: the injected seam."""
        return self._session.async_w3 if self._session is not None else self._injected_async_w3

    @property
    def dispatcher(self) -> Dispatcher | None:
        """The session's dispatcher. Before ``start()``: ``None`` (no session yet)."""
        return self._session.dispatcher if self._session is not None else None

    # ── Phase A: pre-resume startup ─────────────────────────────────
    async def start(self) -> BotRunner:
        """Build the actors, fetch block state, load snapshots, run ``engine_registry.start()``.

        Stops at ``Backfilled`` — BEFORE ``resume()``. Zero result batches
        emit during this window (the pump isn't running), so ``run()`` can
        attach the consumer in the gap before ``resume()`` without a stale-backlog
        window. Idempotent via the phase alone: re-entry once Started
        is a no-op; Running/Closed re-entry raises :class:`PhaseError`.
        """
        if self._phase is _Phase.STARTED:
            return self
        next_phase = self._phase.on_start()

        cfg = self.cfg

        # Arm the incident-diagnostic harnesses (tracemalloc diff thread,
        # /proc RSS CSV sampler, faulthandler repeat dump) from the typed
        # config — every cockpit-driven entrypoint gets them uniformly, and
        # env reads stay the loader's. Zero-config arms nothing.
        arm_diagnostics(cfg.diag)

        # ── Build the three actors (injected or from cfg) ──
        bot, async_w3, engine_registry = await self._build_actors(cfg)

        # ── Fetch current block (for the dispatcher + backfill comparison) ──
        # Note: main()'s start-phase base_fee_next/operator_nonce fetches are
        # dead state — they are recomputed per-batch inside consume_result_batches.
        latest_block = await async_w3.get_block("latest")
        if latest_block is None:
            msg = "Failed to fetch the latest block at session start"
            raise RuntimeError(msg)
        current_block = latest_block["number"]

        # ── Coordination state ──
        dispatcher = Dispatcher.for_block(current_block)

        # Register the operator-verified standard-ERC-20 set as a hard
        # classifier invariant: if the FoT registry ever confirms one of
        # these, the driver panics rather than silently dropping that token's
        # real arbitrage (coarse guard, not an exemption).
        dispatcher.set_fot_verified_non_fot(list(ETH_MAINNET_ALLOWED_TOKENS))

        sim_ctx = self._build_sim_ctx(async_w3, cfg, engine_registry)

        # ── Snapshots (V3 pool tracker pre-population only; the engine's DB
        # snapshot is loaded eagerly at Bot construction via
        # `Bot::load_snapshot_from_db`, and the snapshot→WS gap closes in
        # `resume_from_subscribe`). `engine_registry.start()` takes
        # `v3_snapshot`/`v4_snapshot` kwargs ONLY when the snapshots are
        # non-DB (file/memory) — the `_injected` fast path. The production
        # DB path reads the snapshot at construction and `start()` takes no
        # snapshot kwargs.
        v3_snap, v4_snap, start_v3, start_v4 = self._start_snapshots(bot)
        self.v3_snapshot = v3_snap
        self.v4_snapshot = v4_snap

        # ── Engine pre-resume ritual (subscribe → verify) ──
        # The snapshot→WS gap is closed automatically inside
        # `BlockPump::resume_from_subscribe` at resume. `start()` only
        # subscribes + sets up verify config; resume drives both the backfill
        # and the live loop. Non-DB snapshots flow through `load_*_from_py`
        # in `start()`; the DB path takes no kwargs (snapshot loaded at
        # construction; `snapshot_seed_block` is read from the core
        # `BotState` by `start()` via the `snapshot_seed_block` getter).
        backfill_target = engine_registry.start(
            cfg.node_http,
            cfg.node_ws,
            v3_snapshot=start_v3,
            v4_snapshot=start_v4,
            verify_state_view=EthereumMainnetUniswapV4.state_view.address,
        )
        if backfill_target > current_block:
            current_block = backfill_target
            dispatcher.advance_block(backfill_target)

        # ── THE session: real from here on — the one owner of the
        # coordination state. The runner keeps no stored copies (its
        # same-named attributes are the facade over this owner); the two
        # pipelines attach later through the owner's mutators.
        self._session = _SessionState(
            engine_registry=engine_registry,
            async_w3=async_w3,
            sim_ctx=sim_ctx,
            dispatcher=dispatcher,
            cfg=cfg,
            current_block=current_block,
            bot=bot,
            relay_posture=(
                self._injected_relay_posture
                if self._injected_relay_posture is not None
                else self._resolve_relay_posture(live=not cfg.dry_run)
            ),
        )
        self._install_sigint_handler()
        self._phase = next_phase
        return self

    async def _build_actors(
        self, cfg: ArbitrageConfig
    ) -> tuple[Bot, AsyncAlloyProvider, EngineRegistry]:
        """Build (or adopt the injected) actor trio for a fresh session."""
        bot = self._injected_bot or self._build_bot(cfg)
        async_w3 = self._injected_async_w3 or await self._build_async_w3(cfg)
        engine_registry = self._injected_engine_registry or EngineRegistry(bot=bot)
        return bot, async_w3, engine_registry

    @staticmethod
    def _resolve_relay_posture(*, live: bool) -> RelayPosture | None:
        """The settlement broadcast posture from the resolved typed config.

        In live mode this is the fail-closed boot gate: the Rust readiness
        resolution refuses an activated facet with an unsettled endpoint set
        (naming the `degenbot strategy activate` remedies), and the hosted
        runner IS the settlement arm, so an inactive settlement facet is a
        refusal too — both abort the session instead of degrading a live
        broadcast to the public mempool. Dry-run sessions keep no posture:
        nothing is signed, so no fan-out matters and offline boots stay
        env-free."""

        from degenbot.strategy import settlement_broadcast_endpoints, validate_strategy_readiness

        try:
            validate_strategy_readiness()
        except ValueError as refusal:
            if live:
                raise RuntimeError(f"activation gate refused: {refusal}") from refusal
            return None
        if not live:
            return None
        try:
            relay_urls = settlement_broadcast_endpoints()
        except ValueError as refusal:
            raise RuntimeError(f"settlement arm gate refused: {refusal}") from refusal
        return RelayPosture(relay_urls=relay_urls)

    @staticmethod
    def _build_sim_ctx(
        async_w3: AsyncAlloyProvider,
        cfg: ArbitrageConfig,
        engine_registry: EngineRegistry,
    ) -> SimulateContext | None:
        """Build the session's sim context (``None`` for non-Alloy providers).

        One SimulateContext per session, held alongside the dispatcher. The
        runtime-bytecode file-load stays Python (``stays-python``); the bytes
        cross here. The AsyncAlloyProvider handle is taken from the session's
        provider so ``dispatch_profitable`` shares one provider with the rest
        of the pipeline. Inline-sim wiring rides the same call.
        """
        async_alloy = async_w3.as_async_alloy()
        if async_alloy is None:
            # Non-Alloy provider (test fakes). Defer the sim context:
            # production sessions are Alloy-backed + build it eagerly here;
            # dispatch raises a clear error if reached without one.
            return None
        runtime_code = _load_executor_runtime_bytecode(cfg)
        sim_ctx = SimulateContext(
            provider=async_alloy,
            executor_owner=cfg.executor_owner,
            executor_address=cfg.executor_address,
            weth_address=WETH_ADDRESS,
            pool_manager_address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
            multicall3_address=MULTICALL3_ADDRESS,
            inject_code=cfg.inject_executor_code,
            executor_runtime_bytecode=bytes.fromhex(runtime_code[2:]),
            injected_address=cfg.injected_address if cfg.inject_executor_code else None,
        )
        # The inline-sim stance (`DEGENBOT_SOLVE_INLINE_SIM`) needs the
        # ENGINE hook installed from this session's sim config —
        # without it stance=1 carries no payloads (harmless but inert).
        # Cheap Arc-clone wiring; the engine only calls the hook under the
        # stance, so installing it unconditionally is a no-op when off.
        engine_registry.engine.install_inline_simulator(
            sim_ctx,
            erc6909_profit=cfg.erc6909_profit,
        )
        return sim_ctx

    def _start_snapshots(self, bot: Bot) -> tuple[Any, Any, Any, Any]:
        """Resolve ``(v3, v4, start_v3, start_v4)`` for the pre-resume ritual.

        Injected snapshots (the ``_injected`` fast path) flow through
        ``engine_registry.start()``; the production DB path reads them from the
        bot's store and passes no start kwargs.
        """
        if self._injected_snapshots is not None:
            v3_snap, v4_snap, _v3_blk, _v4_blk = self._injected_snapshots
            return v3_snap, v4_snap, v3_snap, v4_snap
        # Production DB path: snapshot for the V3 pool tracker only
        # (engine feeds from the core store, set at Bot construction).
        v3_snap, v4_snap, _v3_blk, _v4_blk = get_snapshots(bot)
        return v3_snap, v4_snap, None, None

    # ── Phase B: the rolling-start main loop ──────────────────────────
    async def run(self) -> None:
        """Attach the consumer, resume the pump, build paths, release, then run the main loop.

        Ordering (the invariant this session enforces):
        1. create the consumer task (BEFORE resume — closes the stale-backlog window)
        2. ``engine_registry.engine.resume()`` (the single gate after which batches flow)
        3. ``await build_paths(...)`` (rolling start: eager solves dispatch as fresh blocks roll in)
        4. ``bot.release_python_state()`` + drop the bot (hot loop keeps only engine + async_w3)
        5. await the session watch over the main loop (indefinite)
        """
        self._phase = self._phase.on_run()
        # The session's construction answers the actor asserts: the actors
        # are real on the owner the moment start() built it.
        session = self._session
        assert session is not None
        assert session.bot is not None

        cfg = self.cfg
        consumer = self._consumer or consume_result_batches

        # 1. Acquire the once-only block_stream and feed it DIRECTLY to the result
        # consumer (no tee; the Rust two-step gate + solve-time solver-state
        # verifier own verification — no Python whole-batch re-verify). The
        # block-clock pipe is
        # coordinator-owned (ADR-027 completion): `bot.block_stream()` moves
        # the mpsc receiver out of the PumpState on each call — a second call
        # raises RuntimeError("block_stream() can only be called once").
        block_stream = session.bot.block_stream()

        # Attach the consumer BEFORE resume (consumer-safety invariant).
        self._result_consumer_task = asyncio.create_task(
            consumer(session=session, block_stream=block_stream),
            name="result-consumer",
        )
        # Attach the consumer to the session watch the moment it exists — a
        # teardown after any later run() failure (an inline build_paths
        # raise, Ctrl-C during registration) still reaches it.
        self._session_watch.attach(
            consumer_task=self._result_consumer_task,
            watchdog_factory=self._pump_finished_watchdog,
        )

        # 2. Resume the pump — the single gate after which result batches flow.
        session.engine_registry.engine.resume()

        # 3. Build paths with the pump live (rolling start).
        path_builder = self._path_builder or build_paths
        # For the real `build_paths`, build the construction context ONCE
        # here so the registration task owns it — a separate
        # identity from run()'s main-loop state that the trim
        # (`release_python_state()` + `self.bot = None`) never severs. Injected
        # builders (tests) skip context construction (fakes lack the builder
        # surface) and receive `context=None`.
        registration_context = None
        pipeline = None
        if self._path_builder is None:
            self._registration_context = ConstructionContext.for_bot(session.bot, self.v3_snapshot)
            registration_context = self._registration_context
            # Own the long-lived PathRegistrationPipeline on the session so
            # the operator add-a-path surface (enqueue_path /
            # trigger_discovery) stays reachable for the session's lifetime —
            # including after build_paths returns and the main-loop trim drops
            # the Python bot (the pipeline's retained ConstructionContext keeps
            # constructing through the Rust PoolBuilder). Attached through the
            # owner's mutator — its producer legitimately lands here, in run().
            pipeline = PathRegistrationPipeline(
                context=registration_context,
                engine_registry=session.engine_registry,
                retry_policy=cfg.verification_retry_policy,
                max_paths=cfg.max_registered_paths,
                progress_interval_secs=cfg.reg_progress_secs,
            )
            session.attach_registration_pipeline(pipeline)

        # Background registration: decouple discovery from the main loop.
        # PRODUCTION (real `build_paths`): spawn the registration pipeline +
        # its post-completion trim as a background task and enter the main
        # loop immediately. The ConstructionContext keeps the construction
        # resources alive
        # independent of run()'s loop state after the trim. The cross-task
        # fail-fast channel (step 5) surfaces a fatal verification error
        # loudly. INJECTED (tests): await the injected builder synchronously
        # and trim immediately, so the orchestration tests observe the trim
        # deterministically (unchanged behavior).
        background = self._background_registration
        if background is None:
            background = self._path_builder is None
        if background:
            self._registration_task = asyncio.create_task(
                self._run_registration_background(
                    path_builder=path_builder,
                    registration_context=registration_context,
                    retry_policy=cfg.verification_retry_policy,
                    pipeline=pipeline,
                ),
                name="registration-background",
            )
            # The optional registration member joins the watch-set.
            self._session_watch.attach_registration(self._registration_task)
        else:
            await path_builder(
                bot=session.bot,
                engine_registry=session.engine_registry,
                options=BuildPathsOptions(
                    v3_snapshot=self.v3_snapshot,
                    v4_snapshot=self.v4_snapshot,
                    retry_policy=cfg.verification_retry_policy,
                    context=registration_context,
                    pipeline=pipeline,
                    permutation_filter=cfg.permutation_filter,
                ),
            )
            self._trim_python_state()

        # 3b. No startup batch verify — redundant with the per-pool two-step
        # verify and racy at the moving head. Step-1 (seed @ snapshot block) runs
        # inside build_paths for each Tracked pool and proves the snapshot was
        # good; step-2 (post-drain @ backfill block) proves the drain/pump
        # applied buffered events correctly. A whole-batch re-verify at
        # `last_processed_block()` (the live head) would re-check what
        # step-1/step-2 just verified AND race the pump's WS log-application
        # lag: a block's header can advance `last_processed_block()` past it
        # before its Mint log is dispatched (V2-V2-V3 crash at mainnet
        # 25397049: the Mint went unapplied while the cursor advanced). The
        # per-pool gates are race-free (frozen-block pin); in-loop drift
        # detection stays solver-side. The analyzer keys `verify_basis` on the
        # per-pool `[verify-seed]`/`[verify-drain]` lines (see
        # permutation_analyzer._VERIFY_OK_RE).

        # 5. Main loop — runs until the consumer task ends. (No recurring-
        # verify task: in-loop solver-state divergence is owned by the Rust
        # solve-time verifier, not a Python whole-batch re-verify.)
        assert self._result_consumer_task is not None
        try:
            # The session watch owns the main loop's end-state —
            # the watch-set ({consumer} + optional {registration, watchdog}),
            # the SessionEndVerdict ranking (fail-fast beats watchdog in the
            # same batch, written once), and the watchdog drain on exit.
            verdict = await self._session_watch.wait()
        finally:
            # Main loop ended while registration still climbs (shutdown):
            # the watch stops the dangling background task.
            await self._session_watch.teardown_registration()
        if verdict is SessionEndVerdict.RegistrationFailed:
            error = self._session_watch.registration_error
            assert error is not None
            raise error

    # ── Background registration + trim + fail-fast channel ──
    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path at any time (the operator surface).

        Delegates to the session's live :class:`PathRegistrationPipeline`
        (created in :meth:`run`); ``path_steps`` + optional ``directions`` are
        the same shapes as :meth:`PathRegistrationPipeline.enqueue_path`. The
        path is built via the retained ``ConstructionContext`` (Rust
        ``PoolBuilder``), registered + verified, released to ``Live``, and
        registered — without disturbing the pump's update/solve/dispatch.

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders
                have no construction surface, or ``run()`` has not run).
        """
        self._phase = self._phase.on_query("enqueue_path")
        session = self._session
        assert session is not None
        if session.registration_pipeline is None:
            msg = "no live registration pipeline; add-path unavailable (injected/fake run)"
            raise RuntimeError(msg)
        await session.registration_pipeline.enqueue_path(path_steps, directions=directions)

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (on-demand trigger),
        delegating to the session's live pipeline. Returns the number
        of paths processed.

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders,
                or ``run()`` has not run).
        """
        self._phase = self._phase.on_query("trigger_discovery")
        session = self._session
        assert session is not None
        if session.registration_pipeline is None:
            msg = "no live registration pipeline; on-demand discovery unavailable"
            raise RuntimeError(msg)
        return await session.registration_pipeline.trigger_discovery(bound=bound)

    async def _run_registration_background(
        self,
        *,
        path_builder: Callable[..., Awaitable[None]],
        registration_context: ConstructionContext | None,
        retry_policy: VerificationRetryPolicy | None,
        pipeline: Any = None,
    ) -> None:
        """Run ``build_paths`` + the post-completion trim as the background task.

        Production decoupling: called via ``asyncio.create_task`` so the
        main loop starts before discovery completes. ``path_builder`` is the real
        ``build_paths``; after it returns the state-trim runs HERE — not on the
        main-loop entry path — so the trim's clearing of the shared
        tracker/pool/token registries cannot clobber a still-running
        registration (the ``ConstructionContext`` holds the same mutable
        objects). A fatal verification error propagates out of ``build_paths``
        and is surfaced by the step-5 fail-fast channel.

        Cooperative concurrency note: this task runs on the asyncio loop, so it
        interleaves with the consumer only at `await` points (synchronous
        ``build_pool`` FFI calls still briefly occupy the loop thread). The pump
        itself solves on its own tokio thread regardless.
        """
        try:
            await path_builder(
                bot=self.bot,
                engine_registry=self.engine_registry,
                options=BuildPathsOptions(
                    v3_snapshot=self.v3_snapshot,
                    v4_snapshot=self.v4_snapshot,
                    retry_policy=retry_policy,
                    context=registration_context,
                    pipeline=pipeline,
                    permutation_filter=self.cfg.permutation_filter,
                ),
            )
            self._trim_python_state()
        except asyncio.CancelledError:
            # Registration is being torn down mid-flight (cancelled by run()'s
            # finally / a Ctrl-C / a fatal sim trap) BEFORE `build_paths`
            # finished. Registration offloads `assemble_*_tick_map` (which
            # clone the `Arc<SnapshotDb>`) onto a ThreadPoolExecutor;
            # `path_builder`'s futures are NOT awaited/joined here, so worker
            # threads may still be mid-`assemble` holding their clones. Running
            # `close_snapshot_tx()` now would make the `Arc::try_unwrap` canary
            # false-positive with a secondary ``RuntimeError`` that masks the
            # real teardown reason. We're tearing the process down
            # anyway — the WAL snapshot is a process-lifetime concern that
            # becomes moot at exit, so skip the read-tx commit/canary and let
            # the `Arc<SnapshotDb>` drop naturally with `Bot`. The rest of the
            # state trim (release Python registries + drop the bot ref) still
            # runs. The normal-path `_trim_python_state()` directly below keeps
            # the canary fully active for healthy registrations.
            self._trim_python_state(close_read_tx=False)
            raise

    def _trim_python_state(self, *, close_read_tx: bool = True) -> None:
        """Trim redundant Python state once registration is done.

        Shared by the injected-sync and background-registration paths. Releases
        the held snapshot read tx, then drops the Python-side caches and nulls
        run()'s bot ref so the hot loop isn't pinning Python pool objects.

        ``close_read_tx``: on the healthy path (``build_paths`` completed) the
        ``Arc<SnapshotDb>`` canary fires and the read tx is committed to
        reclaim WAL space. On the mid-registration cancel/teardown branch it
        is ``False`` — build-worker ``Arc<SnapshotDb>`` clones may still be
        live, so the canary would false-positive; the tx is instead dropped
        with ``Bot``
        at process teardown. Callers must keep the canary active whenever
        registration actually finished.
        """
        registry = self.engine_registry
        assert registry is not None
        # 3b. Release the held snapshot read transaction:
        # `load_snapshot_from_db` opened a deferred read tx so every
        # `assemble_*_tick_map` Db-arm read during `build_paths` shared one
        # frozen DB snapshot. Pool registration is done — commit the tx to
        # release the WAL snapshot so the updater's checkpoint can reclaim
        # `-wal` space for the hot loop. No-op for the cold-start path (no DB).
        # `getattr` so test fakes (`_FakeBot`) without a real `_py_bot` skip.
        # Skipped entirely on the cancel/teardown branch: in-flight
        # executor `assemble_*` clones would trip the canary, and the WAL is
        # moot once the process is exiting.
        bot = self.bot
        if bot is not None:
            py_bot = getattr(bot, "_py_bot", None)
            if py_bot is not None and close_read_tx:
                py_bot.close_snapshot_tx()

        if bot is None:
            return

        # 4. Trim redundant Python state — Rust engine owns canonical pool state.
        bot.release_python_state()
        self.v3_snapshot = None
        self.v4_snapshot = None
        self.bot = None  # drop the only Python ref; engine keeps its own Bot ref
        gc.collect()
        self._injected_bot = None  # release the injected ref too

        bot_logger.info(
            f"[startup] State trimmed — "
            f"{registry.engine.v2_pool_count()} V2, "
            f"{registry.engine.v3_pool_count()} V3, "
            f"{registry.engine.v4_pool_count()} V4 pools retained in "
            f"Rust engine; {registry.engine.path_count()} paths registered. "
            f"Entering main loop.",
        )

    async def _pump_finished_watchdog(self) -> None:
        """Await the Rust pump's completion event, then cancel the consumer.

        The hotpath timed exit (``HOTPATH_SHUTDOWN_MS``) makes the pump return
        normally after writing its report. Without this watcher the runner's
        idle consumer task keeps the process alive forever on a dead engine —
        ``run_bot.sh`` then reports "running" while nothing progresses (the
        post-unwind wedge: gil-probe idle for minutes after the pump exited).
        The completion surface also fires on a pump panic (the pump task owns
        the channel's only sender; unwinding drops it), so this doubles as a
        panic fail-safe.

        The await is the real Rust-backed completion future for every engine —
        there is no injected-engine/no-surface bypass: test doubles satisfy the
        same awaitable contract (a fake pump that never finishes parks here
        forever, which is exactly the pre-finish consumer shape).

        Returns once the pump finished; the consumer was cancelled here, so the
        session watch observes the pump end as ``WatchdogTripped``.
        """
        registry = self.engine_registry
        assert registry is not None
        await registry.engine.pump_finished_future()
        bot_logger.warning(
            "[shutdown] pump task completed outside stop() — "
            "cancelling the consumer for a graceful teardown"
        )
        main_task = self._result_consumer_task
        if main_task is not None and not main_task.done():
            main_task.cancel()

    # ── Actor builders (production path — only used when not injected) ──
    @staticmethod
    def _build_bot(cfg: ArbitrageConfig) -> Bot:
        config_obj = _make_arbitrage_config(cfg.node_http)
        # ADR-005: the Bot's build path (ERC20 + V2/V3/V4 pool construction)
        # issues many `eth_call`s via `BotIo` → `provider.call`. A web3.py
        # sync backend (`from_web3`) holds the GIL through every
        # `requests.post` on the event-loop thread, starving the asyncio loop
        # during `build_paths`. Use the Rust `AlloyProvider` instead —
        # `PyAlloyProvider.call` releases the GIL (`py.detach`) and does HTTP
        # in Rust, so the pump/consumer can proceed and RPC is faster.
        alloy = AlloyProvider(cfg.node_http)
        return Bot(config_obj, provider=alloy)

    @staticmethod
    async def _build_async_w3(cfg: ArbitrageConfig) -> AsyncAlloyProvider:
        """Build the dispatch-path RPC provider.

        Returns an ``AsyncAlloyProvider`` wrapping a Rust
        ``AsyncAlloyProvider`` — every dispatch-side ``eth_*`` call the hot
        loop makes goes through Rust (releasing the GIL), not raw
        ``AsyncWeb3(AsyncHTTPProvider(...))``. The two typed calls
        (``eth_feeHistory`` / ``eth_sendRawTransaction``) route via
        ``make_request`` on the alloy backend; the generic ones
        (``get_block`` / ``get_transaction_count`` /
        ``eth_call`` / ``get_code`` / ``get_transaction_receipt``) route via
        the adapter's typed methods.

        Returns:
            An ``AsyncAlloyProvider`` (alloy backend) for the dispatch path.

        """
        return await AsyncAlloyProvider.create(cfg.node_http)

    # ── Async context manager ────────────────────────────────────────
    async def __aenter__(self) -> Self:
        """Start the pump, then hand the started session back to the ``async with`` block."""
        await self.start()
        return self

    def _install_sigint_handler(self) -> None:
        """Bind a SIGINT handler that stops the Rust pump *immediately*.

        The ``__aexit__`` → ``shutdown()`` → ``engine.stop()`` path only fires
        once the awaited coroutine unwinds — and during ``build_paths`` the
        main thread is blocked inside the synchronous ``find_paths`` graph
        prep / the Rust ``find_paths_rust`` DFS. Python's default SIGINT →
        raise ``KeyboardInterrupt`` mechanism is *deferred* until that section
        yields control to the eval loop, so the first Ctrl-C appeared to be
        swallowed: the pump (on the shared tokio runtime, a separate thread)
        kept running, the operator pressed Ctrl-C again, and only when
        ``find_paths`` finally returned did the deferred exception unwind to
        ``__aexit__`` and stop the pump.

        Installing this handler closes the gap: the moment SIGINT arrives,
        ``engine.stop()`` runs (it just sets the shutdown flag + aborts the
        pump task — cheap, GIL-only, idempotent) regardless of what the main
        thread is doing. The Rust ``find_paths_rust`` DFS releases the GIL via
        ``py.detach()``, so the handler *can* run even mid-DFS. We then
        re-raise so the normal ``KeyboardInterrupt`` unwind proceeds to
        ``__aexit__`` (which runs ``shutdown()`` again — a no-op — for the
        consumer cancellation).

        Idempotent: if already installed (or if ``signal`` can't bind — e.g. a
        non-main thread), it's a no-op so the call site in ``start()`` is safe
        to re-enter.
        """
        if self._sigint_installed or not self._install_sigint:
            return
        engine = self.engine_registry.engine if self.engine_registry is not None else None
        if engine is None:
            return
        try:
            self._previous_sigint_handler = signal.getsignal(signal.SIGINT)
        except ValueError:
            # `signal.signal` only works on the main thread; if start() is
            # ever driven off-thread there is nothing to bind — rely on
            # __aexit__'s shutdown() alone.
            return

        signal.signal(signal.SIGINT, self._handle_sigint)
        self._sigint_installed = True

    def _stop_engine(self) -> None:
        """The ONE engine-stop entrypoint (``shutdown()`` + SIGINT funnel here).

        Mirrors the Rust ``stop()`` contract: idempotent, sets the shutdown
        flag, and aborts the pump task. Best-effort — a torn-down engine during
        a partial startup must not mask the in-flight exception.
        """
        registry = self.engine_registry
        engine = registry.engine if registry is not None else None
        if engine is None:
            return
        try:
            engine.stop()
        except Exception as exc:
            bot_logger.warning(f"[shutdown] engine.stop() failed: {exc!r}")

    def _handle_sigint(self, _signum: int, _frame: object) -> None:
        """Stop the pump through the one funnel the instant SIGINT arrives.

        Bound (not a closure) so the funnel is a named, testable seam. Fires
        even while the main thread is blocked in ``find_paths`` (the Rust DFS
        releases the GIL), then re-raises ``KeyboardInterrupt`` so the awaiting
        coroutine unwinds through ``__aexit__`` → ``shutdown()`` (idempotent).
        """
        self._stop_engine()
        raise KeyboardInterrupt

    def _restore_sigint_handler(self) -> None:
        if not self._sigint_installed:
            return
        with contextlib.suppress(ValueError, TypeError):
            signal.signal(signal.SIGINT, cast("Any", self._previous_sigint_handler))
        self._sigint_installed = False

    async def __aexit__(self, *exc: object) -> None:
        """Best-effort cleanup; never suppresses.

        Signals the Rust pump to stop, then cancels the consumer task so no
        hanging background task outlives the session. ``shutdown()`` is
        best-effort: it swallows any error from the Rust ``stop()`` so a
        torn-down engine during a partial startup can't mask the original
        exception (the one this ``__aexit__`` is unwinding).

        Ordering rationale: the pump must be stopped BEFORE the consumer task
        is cancelled. The consumer awaits ``engine.__anext__()`` which blocks
        on the pump's result channel; cancelling the consumer first leaves the
        pump's WS task running on the shared tokio runtime, blocking process
        exit until the WS subscription closes itself (up to 60s on a silent
        stream). Stopping the pump first closes the channels → the consumer's
        next ``__anext__`` raises ``StopAsyncIteration`` → the consumer task
        ends cleanly, and the awaited consumer task returns without needing the
        ``CancelledError`` path in the common case.
        """
        await self.shutdown()
        self._restore_sigint_handler()
        # The consumer-cancel duty lives on the session watch — its
        # teardown() re-checks the registration + watchdog drains (idempotent
        # no-ops once run() unwound) and cancels the consumer AFTER the pump
        # was stopped above (the ordering rationale in this docstring).
        await self._session_watch.teardown()

    async def shutdown(self) -> None:
        """Signal the Rust core to stop the pump (best-effort).

        Safe to call at any point in the lifecycle — before ``start()`` finished
        (``engine_registry`` may be ``None``), after ``run()`` exited, or from a
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
        # may reach here at any point; idempotent by design, and the Rust
        # host's SessionPhase table owns the verdict.
        self._phase = self._phase.on_shutdown()
        self._stop_engine()
        # ADR-043 §6: flush the telemetry providers BEFORE this runner — and
        # the Rust core's tokio runtime behind it — is torn down. An OTLP batch
        # flushed after runtime teardown exports nothing, so the tail of a run
        # would be silently lost. None-safe when telemetry is off.
        try:
            from degenbot.telemetry import flush_telemetry as _flush_telemetry

            _flush_telemetry()
        except Exception as exc:
            bot_logger.debug(f"[shutdown] telemetry flush failed: {exc!r}")


# get_snapshots


def get_snapshots(
    bot: Bot,
) -> tuple[
    UniswapV3LiquiditySnapshot | None,
    UniswapV4LiquiditySnapshot | None,
    int | None,
    int | None,
]:
    """Load V3 and V4 liquidity snapshots from the database for the V3 pool
    tracker pre-population.

    The engine's DB snapshot is loaded eagerly at `Bot` construction by
    `Bot::load_snapshot_from_db`, and the snapshot→WS gap is closed
    automatically inside `BlockPump::resume_from_subscribe` — so these
    snapshots feed only the V3 pool tracker, not the engine.

    Returns (v3_snapshot, v4_snapshot, v3_snapshot_block, v4_snapshot_block).
    """
    v3_snapshot_block: int | None = None
    v4_snapshot_block: int | None = None

    # ── V3 snapshot ──────────────────────────────────────────────
    v3_snapshot = None
    try:
        v3_snapshot = UniswapV3LiquiditySnapshot(
            source=V3DatabaseSnapshot(chain_id=1, db=bot.db),
        )
    except ValueError:
        bot_logger.info("[backfill] V3: no snapshot data in database, skipping")

    if v3_snapshot is not None:
        v3_snapshot_block = v3_snapshot.newest_block
        bot_logger.info(f"[backfill] V3: DB snapshot at block {v3_snapshot_block}")

    # ── V4 snapshot ──────────────────────────────────────────────
    v4_snapshot = None
    try:
        v4_db_snapshot = V4DatabaseSnapshot(chain_id=1, db=bot.db)
        v4_snapshot = UniswapV4LiquiditySnapshot(source=v4_db_snapshot)
    except ValueError:
        bot_logger.info("[backfill] V4: no snapshot data in database, skipping")

    if v4_snapshot is not None:
        v4_snapshot_block = v4_snapshot.newest_block
        bot_logger.info(f"[backfill] V4: DB snapshot at block {v4_snapshot_block}")

    return v3_snapshot, v4_snapshot, v3_snapshot_block, v4_snapshot_block
