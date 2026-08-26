"""Settlement-arbitrage runtime driver facade (``BotRunner``).

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` (epic 5TSYKN, task
DKUOBL; renamed ``BackrunSession`` -> ``BotRunner``). ``BotRunner`` is the
Python-companion cockpit over the Rust-owned engine: it owns the config + the
three actors (``bot``, ``engine_registry``, ``async_w3``) + the ``Dispatcher``
+ scalar block state, and is the one place that enforces the phase ordering the
engine's state machine requires.

    start():  subscribe -> stream snapshots -> backfill -> verify config
              (``EngineRegistry.start``, stops at Backfilled, pre-resume)
    run():    attach consumer -> ``resume()`` -> registration -> main loop

The driver is ``stays-python`` (asyncio loop, SIGINT, deployment policy): it
controls the Rust engine but owns no pool state (ADR-003: ``Bot`` is the
single state owner; ADR-006: ``Bot`` is the per-chain orchestrator). It
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
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Self, cast

from degenbot import Bot
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
)
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.dispatch import Dispatcher, SimulateContext
from degenbot.logging import logger as bot_logger
from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.runner._consume import consume_result_batches
from degenbot.runner._dispatch import _load_executor_runtime_bytecode
from degenbot.runner._driver_constants import (
    ETH_MAINNET_ALLOWED_TOKENS,
    INJECT_EXECUTOR_CODE,
    INJECTED_EXECUTOR_ADDRESS,
    MULTICALL3_ADDRESS,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
from degenbot.runner.build_paths import ConstructionContext, PathRegistrationPipeline, build_paths
from degenbot.runner.config import ArbitrageConfig
from degenbot.uniswap.deployments import EthereumMainnetUniswapV4
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

# _make_arbitrage_config


def _make_arbitrage_config(node_http: str) -> DegenbotConfig:
    """Build a single-chain DegenbotConfig for the arbitrage session (ADR-006 D5).

    The chain identity is Ethereum mainnet (1); the RPC is the caller's
    ``node_http`` — the cascade-resolved endpoint from
    :func:`degenbot.config.resolve_rpc_uris` (CLI > OS env
    ``DEGENBOT_RPC_HTTP_CHAINID_1`` > legacy ``NODE_HOST_*`` > config.toml
    ``rpc[1]``). When config.toml was the winning source, ``node_http`` already
    equals ``rpc[1]``, so the injection here is consistent rather than a bypass.
    The Bot enforces the connected RPC's ``eth_chainId`` matches at construction.

    The database path is read from the existing user config at
    ``~/.config/degenbot/config.toml`` (so locally-configured DB paths are
    honored) and falls back to the default path if no config exists.
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

    return DegenbotConfig(
        database=DatabaseSettings(path=Path("~/.config/degenbot/degenbot.db").expanduser()),
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
    ``start()`` is an idempotent no-op until the session is ``Running``;
    ``run()`` requires ``Started``; ``enqueue_path`` / ``trigger_discovery``
    require ``Running``; ``shutdown()`` stays deliberately any-phase and
    idempotent (the SIGINT teardown ordering depends on it).
    """


class _Phase(Enum):
    """Cockpit session phase (private; the public signal is :class:`PhaseError`)."""

    NEW = "new"
    STARTED = "started"
    RUNNING = "running"
    CLOSED = "closed"


@dataclass
class _SessionState:
    """Cockpit session state (CONTEXT.md term: *session state*).

    The single owner of one pump session's coordination state: the block
    loop (``consume``) and the dispatch leaf (``dispatch``) both read the
    same owner instead of the session travelling as a parameter bag.
    Mutable pieces (``current_block``) advance on the owner.
    """

    engine_registry: EngineRegistry
    async_w3: AsyncAlloyProvider
    sim_ctx: SimulateContext | None
    dispatcher: Dispatcher
    cfg: ArbitrageConfig
    current_block: int


class BotRunner:
    """Orchestrator that collapses the settlement-arbitrage startup ritual behind one facade.

    Owns the config + the three actors (``bot``, ``engine_registry``, ``async_w3``)
    + the ``Dispatcher`` + scalar block state, and is the ONE place that
    enforces the phase ordering the engine's state machine requires:

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

    In production (Sub-B) ``run()`` spawns discovery+registration as a
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
        bot: Bot | None = None,
        engine_registry: EngineRegistry | None = None,
        async_w3: AsyncAlloyProvider | None = None,
        snapshots: tuple[Any, Any, Any, Any] | None = None,
        path_builder: Any = None,
        consumer: Any = None,
        install_sigint: bool = True,
        background_registration: bool | None = None,
    ) -> None:
        """Store config + injectable test actors; the real actors are built in ``start()``.

        ``background_registration`` (default ``None`` → auto) controls the Sub-B
        seam: when ``True`` ``run()`` spawns discovery+registration as a
        background task (decoupled from the main loop, cross-task fail-fast);
        when ``False`` it awaits the path builder synchronously + trims
        immediately (legacy orchestration, used by tests). ``None`` auto-selects
        ``False`` for injected ``path_builder`` (tests) and ``True`` for the real
        ``build_paths`` (production).
        """
        self.cfg = cfg
        self._injected_bot = bot
        self._injected_engine_registry = engine_registry
        self._injected_async_w3 = async_w3
        self._injected_snapshots = snapshots
        self._path_builder = path_builder
        self._consumer = consumer
        self._background_registration: bool | None = background_registration
        # Sub-A seam: registration-owned construction context (built in run()
        # for the real build_paths; None for injected builders and until run()).
        self._registration_context: ConstructionContext | None = None
        # (Sub-B/6VZN7H) + the trim. Owned by run() for the real build_paths;
        # None until run() with real build_paths (injected fakes have no
        # construction surface).
        self._pipeline: Any = None
        # Sub-B seam: the background registration task (production + explicit
        # ``background_registration=True``), awaited for fail-fast in step 5.
        self._registration_task: asyncio.Task | None = None
        # Resolved in start():
        self.bot: Bot | None = None
        self.engine_registry: EngineRegistry | None = None
        self.async_w3: AsyncAlloyProvider | None = None
        self.dispatcher: Dispatcher | None = None
        self._sim_ctx: SimulateContext | None = None
        self.v3_snapshot: Any = None
        self.v4_snapshot: Any = None
        self.current_block: int = 0
        self._phase: _Phase = _Phase.NEW
        self._session: _SessionState | None = None
        self._started = False
        # Created in run():
        self._result_consumer_task: asyncio.Task | None = None
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

    # ── Phase A: pre-resume startup ─────────────────────────────────
    async def start(self) -> BotRunner:
        """Build the actors, fetch block state, load snapshots, run ``engine_registry.start()``.

        Stops at ``Backfilled`` — BEFORE ``resume()``. Zero result batches
        emit during this window (the pump isn't running), so ``run()`` can
        attach the consumer in the gap before ``resume()`` without a stale-backlog
        window. Idempotent guard via ``_started``.
        """
        if self._started:
            return self
        if self._phase is _Phase.RUNNING or self._phase is _Phase.CLOSED:
            msg = f"start() in phase {self._phase.value!r} - session can only start from New"
            raise PhaseError(msg)
        self._started = True

        cfg = self.cfg

        # ── Build the three actors (injected or from cfg) ──
        self.bot = self._injected_bot or self._build_bot(cfg)
        self.async_w3 = self._injected_async_w3 or await self._build_async_w3(cfg)
        self.engine_registry = self._injected_engine_registry or EngineRegistry(bot=self.bot)

        # ── Fetch current block (for the dispatcher + backfill comparison) ──
        # Note: main()'s start-phase base_fee_next/operator_nonce fetches were
        # dead state (recomputed per-batch inside consume_result_batches) — dropped.
        latest_block = await self.async_w3.get_block("latest")
        if latest_block is None:
            msg = "Failed to fetch the latest block at session start"
            raise RuntimeError(msg)
        self.current_block = latest_block["number"]

        # ── Coordination state ──
        self.dispatcher = Dispatcher.for_block(self.current_block)

        # Register the operator-verified standard-ERC-20 set as a hard
        # classifier invariant: if the FoT registry ever confirms one of
        # these, the driver panics rather than silently dropping that token's
        # real arbitrage (coarse guard, not an exemption).
        self.dispatcher.set_fot_verified_non_fot(list(ETH_MAINNET_ALLOWED_TOKENS))

        # ── Simulation seam context (A5) — one SimulateContext per session,
        # held alongside the dispatcher. The runtime-bytecode file-load stays
        # Python (A2 disposition `stays-python`); the bytes cross here. The
        # AsyncAlloyProvider handle is taken from the session's provider so
        # `dispatch_profitable` shares one provider with the rest of the
        # pipeline.
        async_alloy = self.async_w3.as_async_alloy()
        if async_alloy is None:
            # Non-Alloy provider (test fakes). Defer the sim context:
            # production sessions are Alloy-backed + build it eagerly here;
            # dispatch raises a clear error if reached without one.
            self._sim_ctx = None
        else:
            runtime_code = _load_executor_runtime_bytecode(cfg)
            self._sim_ctx = SimulateContext(
                provider=async_alloy,
                executor_owner=cfg.executor_owner,
                executor_address=cfg.executor_address,
                weth_address=WETH_ADDRESS,
                pool_manager_address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
                multicall3_address=MULTICALL3_ADDRESS,
                inject_code=INJECT_EXECUTOR_CODE,
                executor_runtime_bytecode=bytes.fromhex(runtime_code[2:]),
                injected_address=INJECTED_EXECUTOR_ADDRESS if INJECT_EXECUTOR_CODE else None,
            )

        # ── Snapshots (V3 pool tracker pre-population only; the engine's DB
        # snapshot is loaded eagerly at Bot construction via
        # `Bot::load_snapshot_from_db` — JUCFCB, Shape 2 — and the
        # snapshot→WS gap closes in `resume_from_subscribe` — J3FMDO).
        # `engine_registry.start()` takes `v3_snapshot`/`v4_snapshot` kwargs
        # ONLY when the snapshots are non-DB (file/memory) — the `_injected`
        # fast path. The production DB path reads the snapshot at
        # construction and `start()` takes no snapshot kwargs (the retired
        # DB-snapshot `stream_*_to_engine` SQLAlchemy forwarding is gone —
        # JUCFCB/2SM4Y7).
        v3_snap: Any = None
        v4_snap: Any = None
        start_v3 = None  # snapshots passed to `start()` (non-DB only)
        start_v4 = None
        if self._injected_snapshots is not None:
            v3_snap, v4_snap, _v3_blk, _v4_blk = self._injected_snapshots
            start_v3, start_v4 = v3_snap, v4_snap
        else:
            # Production DB path: snapshot for the V3 pool tracker only
            # (engine feeds from the core store, set at Bot construction).
            v3_snap, v4_snap, _v3_blk, _v4_blk = get_snapshots(self.bot)
        self.v3_snapshot = v3_snap
        self.v4_snapshot = v4_snap

        # ── Engine pre-resume ritual (subscribe → verify) ──
        # J3FMDO: the snapshot→WS gap is closed automatically inside
        # `BlockPump::resume_from_subscribe` at resume. `start()` only
        # subscribes + sets up verify config; resume drives both the backfill
        # and the live loop. Non-DB snapshots flow through `load_*_from_py`
        # in `start()`; the DB path takes no kwargs (snapshot loaded at
        # construction; `snapshot_seed_block` is read from the core
        # `BotState` by `start()` via the `snapshot_seed_block` getter).
        backfill_target = self.engine_registry.start(
            cfg.node_http,
            cfg.node_ws,
            v3_snapshot=start_v3,
            v4_snapshot=start_v4,
            verify_state_view=EthereumMainnetUniswapV4.state_view.address,
        )
        if backfill_target > self.current_block:
            self.current_block = backfill_target
            self.dispatcher.advance_block(backfill_target)

        assert self.engine_registry is not None
        assert self.async_w3 is not None
        assert self.dispatcher is not None
        self._session = _SessionState(
            engine_registry=self.engine_registry,
            async_w3=self.async_w3,
            sim_ctx=self._sim_ctx,
            dispatcher=self.dispatcher,
            cfg=cfg,
            current_block=self.current_block,
        )
        self._install_sigint_handler()
        self._phase = _Phase.STARTED
        return self

    # ── Phase B: the rolling-start main loop ──────────────────────────
    async def run(self) -> None:
        """Attach the consumer, resume the pump, build paths, release, then run the main loop.

        Ordering (the invariant this session enforces):
        1. create the consumer task (BEFORE resume — closes the stale-backlog window)
        2. ``engine_registry.engine.resume()`` (the single gate after which batches flow)
        3. ``await build_paths(...)`` (rolling start: eager solves dispatch as fresh blocks roll in)
        4. ``bot.release_python_state()`` + drop the bot (hot loop keeps only engine + async_w3)
        5. ``await result_consumer_task`` (the main loop, indefinite)
        """
        if self._phase is not _Phase.STARTED:
            msg = f"run() requires phase 'started' (session phase is {self._phase.value!r})"
            raise PhaseError(msg)
        self._phase = _Phase.RUNNING
        assert self.engine_registry is not None
        assert self.async_w3 is not None
        assert self.bot is not None
        assert self.dispatcher is not None

        cfg = self.cfg
        consumer = self._consumer or consume_result_batches

        # 1. Acquire the once-only block_stream and feed it DIRECTLY to the result
        # consumer (no tee, no recurring-verify branch — the redundant Python
        # whole-batch re-verify was removed; the Rust two-step gate + solve-time
        # solver-state verifier own verification). The block-clock pipe is
        # coordinator-owned (ADR-027 completion): `bot.block_stream()` moves
        # the mpsc receiver out of the PumpState on each call — a second call
        # raises RuntimeError("block_stream() can only be called once").
        block_stream = self.bot.block_stream()

        # Attach the consumer BEFORE resume (consumer-safety invariant).
        assert self._session is not None
        self._result_consumer_task = asyncio.create_task(
            consumer(session=self._session, block_stream=block_stream),
            name="result-consumer",
        )

        # 2. Resume the pump — the single gate after which result batches flow.
        self.engine_registry.engine.resume()

        # 3. Build paths with the pump live (rolling start).
        path_builder = self._path_builder or build_paths
        # Sub-A seam: for the real `build_paths`, build the construction
        # context ONCE here so the registration task owns it — a separate
        # identity from run()'s main-loop state that the trim
        # (`release_python_state()` + `self.bot = None`) never severs. Injected
        # builders (tests) skip context construction (fakes lack the builder
        # surface) and receive `context=None`.
        registration_context = None
        pipeline = None
        if self._path_builder is None:
            self._registration_context = ConstructionContext.for_bot(self.bot, self.v3_snapshot)
            registration_context = self._registration_context
            # NWTUM3: own the long-lived PathRegistrationPipeline on the session
            # so the operator add-a-path surface (enqueue_path /
            # trigger_discovery) stays reachable for the session's lifetime —
            # including after build_paths returns and the main-loop trim drops
            # the Python bot (the pipeline's retained ConstructionContext keeps
            # constructing through the Rust PoolBuilder).
            self._pipeline = PathRegistrationPipeline(
                context=registration_context,
                engine_registry=self.engine_registry,
                retry_policy=cfg.verification_retry_policy,
            )
            pipeline = self._pipeline

        # Sub-B seam: decouple discovery from the main loop. PRODUCTION (real
        # `build_paths`): spawn the registration pipeline + its post-completion
        # trim as a background task and enter the main loop immediately. The
        # ConstructionContext (Sub-A) keeps the construction resources alive
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
        else:
            await path_builder(
                bot=self.bot,
                engine_registry=self.engine_registry,
                v3_snapshot=self.v3_snapshot,
                v4_snapshot=self.v4_snapshot,
                retry_policy=cfg.verification_retry_policy,
                context=registration_context,
                pipeline=pipeline,
                permutation_filter=cfg.permutation_filter,
            )
            self._trim_python_state()

        # 3b. STARTUP batch verify REMOVED — redundant with the per-pool two-step
        # verify and racy at the moving head. Step-1 (seed @ snapshot block) runs
        # inside build_paths for each Tracked pool and proves the snapshot was
        # good; step-2 (post-drain @ backfill block) proves the drain/pump
        # applied buffered events correctly. Re-verifying the whole batch at
        # `last_processed_block()` (the live head) re-checked what step-1/step-2
        # just verified AND raced the pump's WS log-application lag: a block's
        # header can advance `last_processed_block()` past it before its Mint
        # log is dispatched (V2-V2-V3 crash — Mint at 25397047 unapplied when
        # 25397049's header advanced the cursor, false-mismatching tick
        # -887270). The per-pool gates are race-free (frozen-block pin); the
        # T7 recurring-verify carries in-loop drift detection. The analyzer
        # now keys `verify_basis` on the per-pool `[verify-seed]`/`[verify-drain]`
        # lines (see permutation_analyzer._VERIFY_OK_RE).

        # 5. Main loop — runs until the consumer task ends. (The recurring-
        # verify task T7 was REMOVED: it redundantly re-checked the whole pool
        # set that already passed the Rust two-step seed/drain gate, and its raw-
        # head anchor caused sporadic false liquidity-map mismatches at the
        # moving head. In-loop solver-state divergence is owned by the Rust
        # solve-time verifier, not a Python whole-batch re-verify.)
        assert self._result_consumer_task is not None
        try:
            if self._registration_task is not None:
                await self._await_main_loop_with_registration_fail_fast()
            else:
                await self._await_main_loop_with_pump_watchdog()
        finally:
            registration_task = self._registration_task
            if (
                registration_task is not None
                and not registration_task.done()
                and not registration_task.cancelled()
            ):
                # Main loop ended while registration still climbs (shutdown):
                # stop the dangling background task.
                registration_task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await registration_task

    # ── Sub-B: background registration + trim + fail-fast channel ──
    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path at any time (NWTUM3 / D1c operator surface).

        Delegates to the session's live :class:`PathRegistrationPipeline`
        (created in :meth:`run`); ``path_steps`` + optional ``directions`` are
        the same shapes as :meth:`PathRegistrationPipeline.enqueue_path`. The
        path is built via the retained ``ConstructionContext`` (Rust
        ``PoolBuilder``), registered + verified, released to ``Live`` per D4,
        and registered — without disturbing the pump's update/solve/dispatch.

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders
                have no construction surface, or ``run()`` has not run).
        """
        if self._phase is not _Phase.RUNNING:
            msg = f"enqueue_path() needs 'running' (phase is {self._phase.value!r})"
            raise PhaseError(msg)
        if self._pipeline is None:
            msg = "no live registration pipeline; add-path unavailable (injected/fake run)"
            raise RuntimeError(msg)
        await self._pipeline.enqueue_path(path_steps, directions=directions)

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (NWTUM3 / D1c on-demand
        trigger), delegating to the session's live pipeline. Returns the number
        of paths processed.

        Raises:
            RuntimeError: if no live pipeline exists (injected fake builders,
                or ``run()`` has not run).
        """
        if self._phase is not _Phase.RUNNING:
            msg = f"trigger_discovery() needs 'running' (phase is {self._phase.value!r})"
            raise PhaseError(msg)
        if self._pipeline is None:
            msg = "no live registration pipeline; on-demand discovery unavailable"
            raise RuntimeError(msg)
        return await self._pipeline.trigger_discovery(bound=bound)

    async def _run_registration_background(
        self,
        *,
        path_builder: Callable[..., Awaitable[None]],
        registration_context: ConstructionContext | None,
        retry_policy: VerificationRetryPolicy | None,
        pipeline: Any = None,
    ) -> None:
        """Run ``build_paths`` + the post-completion trim as the background task.

        Production decoupling (Sub-B): called via ``asyncio.create_task`` so the
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
        itself solves on its own tokio thread regardless; the genuine
        "spawn on the pump tokio runtime" + CPU-level parallelism is the
        Sub-A2-grade Rust port.
        """
        try:
            await path_builder(
                bot=self.bot,
                engine_registry=self.engine_registry,
                v3_snapshot=self.v3_snapshot,
                v4_snapshot=self.v4_snapshot,
                retry_policy=retry_policy,
                context=registration_context,
                pipeline=pipeline,
                permutation_filter=self.cfg.permutation_filter,
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
            # real teardown reason (EZOKDR). We're tearing the process down
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
        XEANMB canary fires and the read tx is committed to reclaim WAL space.
        On the mid-registration cancel/teardown branch it is ``False`` — build-
        worker ``Arc<SnapshotDb>`` clones may still be live, so the canary
        would false-positive (EZOKDR); the tx is instead dropped with ``Bot``
        at process teardown. Callers must keep the canary active whenever
        registration actually finished.
        """
        assert self.engine_registry is not None
        # 3b. Release the held snapshot read transaction (epic XEANMB):
        # `load_snapshot_from_db` opened a deferred read tx so every
        # `assemble_*_tick_map` Db-arm read during `build_paths` shared one
        # frozen DB snapshot. Pool registration is done — commit the tx to
        # release the WAL snapshot so the updater's checkpoint can reclaim
        # `-wal` space for the hot loop. No-op for the cold-start path (no DB).
        # `getattr` so test fakes (`_FakeBot`) without a real `_py_bot` skip.
        # Skipped entirely on the cancel/teardown branch (EZOKDR): in-flight
        # executor `assemble_*` clones would trip the canary, and the WAL is
        # moot once the process is exiting.
        if self.bot is not None:
            py_bot = getattr(self.bot, "_py_bot", None)
            if py_bot is not None and close_read_tx:
                py_bot.close_snapshot_tx()

        if self.bot is None:
            return

        # 4. Trim redundant Python state — Rust engine owns canonical pool state.
        self.bot.release_python_state()
        self.v3_snapshot = None
        self.v4_snapshot = None
        self.bot = None  # drop the only Python ref; engine keeps its own Bot ref
        gc.collect()
        self._injected_bot = None  # release the injected ref too

        bot_logger.info(
            f"[startup] State trimmed — "
            f"{self.engine_registry.engine.v2_pool_count()} V2, "
            f"{self.engine_registry.engine.v3_pool_count()} V3, "
            f"{self.engine_registry.engine.v4_pool_count()} V4 pools retained in "
            f"Rust engine; {self.engine_registry.engine.path_count()} paths registered. "
            f"Entering main loop.",
        )

    async def _await_main_loop_with_registration_fail_fast(self) -> None:
        """Await the consumer (main loop) while watching background registration.

        The registration task (Sub-B) runs discovery+registration concurrently
        with the hot loop. A fatal registration error — `VerificationMismatchError`
        / `VerificationRpcError` (and any other uncaught exception escaping
        ``build_paths``) — must crash loudly: cancel the main-loop consumer and
        re-raise, so the session cannot keep trading on unverified/torn state.
        A clean registration completion is a no-op here (the main loop
        continues; the trim already ran inside the background task).

        If the main loop ends before registration (shutdown), ``run()``'s
        ``finally`` cancels the still-dangling background task.
        """
        main_task = self._result_consumer_task
        assert main_task is not None
        registration_task = self._registration_task
        assert registration_task is not None
        watchdog_task = asyncio.create_task(
            self._pump_finished_watchdog(), name="pump-finished-watchdog"
        )
        pump_ended = False
        watchdog_active = True
        try:
            # The watchdog stays in the watch-set for the WHOLE loop — including
            # after registration completes (only the main loop remains then, but
            # a timed-exit pump can still finish, and must not be missed).
            while not main_task.done():
                watch = {main_task}
                if watchdog_active:
                    watch.add(watchdog_task)
                if registration_task is not None:
                    watch.add(registration_task)
                done, _pending = await asyncio.wait(
                    watch,
                    return_when=asyncio.FIRST_COMPLETED,
                )
                # Fail-fast outranks everything: a fatal registration error must
                # be surfaced even when the watchdog fired in the same wait
                # batch (injected/fake engines return from the watchdog
                # instantly — that completion is NOT a pump end).
                if registration_task is not None and registration_task in done:
                    exc = registration_task.exception()
                    if exc is not None and not isinstance(exc, asyncio.CancelledError):
                        # Fatal registration error → fail loudly: stop the hot loop.
                        main_task.cancel()
                        with contextlib.suppress(asyncio.CancelledError):
                            await main_task
                        raise exc
                    # Registration finished cleanly; stop watching, keep
                    # blocking on {main, watchdog}.
                    registration_task = None
                if watchdog_active and watchdog_task in done:
                    if watchdog_task.result():
                        # Pump finished outside stop() (timed exit / stream end /
                        # abort): the watchdog already cancelled the consumer —
                        # leave via the normal teardown so the process exits.
                        pump_ended = True
                        if registration_task is not None and not registration_task.done():
                            registration_task.cancel()
                        break
                    # No pump-finished surface (injected engine): drop it from
                    # the watch-set instead of misreading instant completion as
                    # a pump end — that would deadlock on an un-cancelled
                    # consumer while swallowing any later registration failure.
                    watchdog_active = False
            if pump_ended:
                with contextlib.suppress(asyncio.CancelledError):
                    await main_task
            else:
                await main_task
        finally:
            watchdog_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await watchdog_task

    async def _await_main_loop_with_pump_watchdog(self) -> None:
        """Await the consumer while watching the pump (inline-registration form).

        When registration runs synchronously (`_registration_task is None`,
        the injected/test seam, and any production path that awaits
        `build_paths` inline), the fail-fast loop above still must notice a
        finished pump. This smaller twin watches {consumer, pump watchdog} and
        tears down gracefully when the pump ends outside `stop()`.
        """
        main_task = self._result_consumer_task
        assert main_task is not None
        watchdog_task = asyncio.create_task(
            self._pump_finished_watchdog(), name="pump-finished-watchdog"
        )
        pump_ended = False
        watchdog_active = True
        try:
            while not main_task.done():
                watch = {main_task}
                if watchdog_active:
                    watch.add(watchdog_task)
                done, _pending = await asyncio.wait(
                    watch,
                    return_when=asyncio.FIRST_COMPLETED,
                )
                if watchdog_active and watchdog_task in done:
                    if watchdog_task.result():
                        # Pump finished; the watchdog already cancelled the
                        # consumer.
                        pump_ended = True
                        break
                    # No pump-finished surface (injected engine): stop watching.
                    # Treating instant completion as a pump end would leave the
                    # un-cancelled consumer as the only pending task — deadlock.
                    watchdog_active = False
            if pump_ended:
                with contextlib.suppress(asyncio.CancelledError):
                    await main_task
            else:
                await main_task
        finally:
            watchdog_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await watchdog_task

    async def _pump_finished_watchdog(self) -> bool:
        """Poll the Rust pump task; when it finishes outside ``stop()``, shut down.

        The hotpath timed exit (``HOTPATH_SHUTDOWN_MS``) makes the pump return
        normally after writing its report. Without a watcher the runner's idle
        consumer task keeps the process alive forever on a dead engine —
        ``run_bot.sh`` then reports "running" while nothing progresses (the
        post-unwind wedge: gil-probe idle for minutes after the pump exited).
        Join-handle completion also covers a pump panic, so this doubles as a
        panic fail-safe. Injected/test engines that lack ``pump_finished``
        simply get no watchdog (no behavioral change on those paths).

        Returns True when a real pump finished outside stop() (the consumer was
        cancelled here), False when there is no pump-finished surface at all —
        callers must NOT treat False as "pump ended", or an injected/fake-engine
        session deadlocks awaiting a consumer nobody cancelled (the fail-fast
        channel must still own that path).
        """
        engine = self.engine_registry.engine if self.engine_registry is not None else None
        pump_finished = getattr(engine, "pump_finished", None) if engine is not None else None
        if pump_finished is None:
            return False
        while True:
            await asyncio.sleep(0.5)
            if pump_finished():
                bot_logger.warning(
                    "[shutdown] pump task completed outside stop() — "
                    "cancelling the consumer for a graceful teardown"
                )
                main_task = self._result_consumer_task
                if main_task is not None and not main_task.done():
                    main_task.cancel()
                return True

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
        # in Rust, so the pump/consumer can proceed and RPC is faster. This
        # is the sync web3.py AlloyProvider being retired.
        alloy = AlloyProvider(cfg.node_http)
        return Bot(config_obj, provider=alloy)

    @staticmethod
    async def _build_async_w3(cfg: ArbitrageConfig) -> AsyncAlloyProvider:
        """Build the dispatch-path RPC provider (PAGQCK).

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

        def _on_sigint(_signum: int, _frame: object) -> None:
            # Stop the pump first — fires even while the main thread is
            # blocked in find_paths (Rust DFS released the GIL). Wrapped
            # because the engine may have been torn down concurrently.
            with contextlib.suppress(Exception):
                engine.stop()
            # Re-raise KeyboardInterrupt so the awaiting coroutine unwinds
            # through __aexit__ → shutdown() (idempotent) + consumer cancel.
            raise KeyboardInterrupt

        signal.signal(signal.SIGINT, _on_sigint)
        self._sigint_installed = True

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
        ends cleanly, and the ``await task`` below returns without needing the
        ``CancelledError`` path in the common case.
        """
        await self.shutdown()
        self._restore_sigint_handler()
        task = self._result_consumer_task
        if task is not None and not task.done():
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await task

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
        # may reach here at any point; idempotent by design.
        self._phase = _Phase.CLOSED
        registry = getattr(self, "engine_registry", None)
        engine = getattr(registry, "engine", None) if registry is not None else None
        if engine is None:
            return
        try:
            engine.stop()
        except Exception as exc:
            bot_logger.warning(f"[shutdown] engine.stop() failed: {exc!r}")


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

    Historically the snapshot also fed `engine_registry.start()` via
    `stream_v3_snapshot_to_engine`/`stream_v4_snapshot_to_engine` SQLAlchemy
    forwarding — that path is retired (JUCFCB/2SM4Y7/DADWUP: the engine's DB
    snapshot is loaded eagerly at `Bot` construction by
    `Bot::load_snapshot_from_db`, and the snapshot→WS gap is closed
    automatically inside `BlockPump::resume_from_subscribe` — J3FMDO; the
    per-pool `insert_*_pool_snapshot` pyo3 surface + the SQLAlchemy
    `yield_per` loops are removed).

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
