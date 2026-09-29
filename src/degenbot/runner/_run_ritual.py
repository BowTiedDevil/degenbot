"""The run ritual: one owner of a running session's startup ordering.

CONTEXT.md *Run ritual*: the cockpit's one owner of a running session's
startup ordering — consumer-attach, watch-attach, resume, registration, main
loop — as a state machine; the registration mode and the task scheduler are
data it consumes, never branches tests select. It sits strictly behind the
``run()`` phase gate and re-legislates no legality the Rust ``SessionPhase``
table already owns.

The host is the :class:`~degenbot.runner.BotRunner` that owns the session:
the ritual reads its collaborators and writes the three run-phase handles
(the consumer/registration task refs and the registration-owned
construction context) onto the session owner, where the runner's other
duties read them. Ritual states are private; diagnostics read facts,
never states.
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from enum import Enum
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage import RetryPolicy
from degenbot.logging import logger as bot_logger
from degenbot.runner._consume import consume_result_batches
from degenbot.runner._session_watch import SessionEndVerdict
from degenbot.runner.build_paths import (
    BuildPathsOptions,
    ConstructionContext,
    PathRegistrationPipeline,
    build_paths,
)

if TYPE_CHECKING:
    from degenbot.runner.bot_runner import BotRunner


class RunRitualError(RuntimeError):
    """A run-ritual sequencing invariant was violated.

    Distinct from the operator-facing :class:`~degenbot.runner.PhaseError`:
    the phase gate (the Rust host's ``SessionPhase`` table) owns lifecycle
    legality, while this error fires only when the ritual's own transitions
    are driven out of order — an internal invariant, unreachable through any
    operator action.
    """


class _RitualState(Enum):
    """The ritual's private states; the order below is the transition table."""

    ATTACH_CONSUMER = "attach_consumer"
    ATTACH_WATCH = "attach_watch"
    RESUME = "resume"
    REGISTRATION = "registration"
    MAIN_LOOP = "main_loop"
    ENDED = "ended"


class RunRitual:
    """The startup-ordering state machine behind ``BotRunner.run()``.

    One legal drive: ``attach_consumer → attach_watch → resume →
    registration → main_loop``. Each named transition admits only its own
    state and refuses every other (:class:`RunRitualError`), so the ordering
    the engine requires lives in the machine rather than in call-site prose:

    - the consumer task exists BEFORE ``resume()`` — the engine's once-only
      result-receiver hand-off is satisfied at engine construction, so the
      residual asyncio-side ordering is that the drain task must exist before
      batches can flow over the unbounded result channel;
    - the session watch holds that consumer before any later failure (an
      inline ``build_paths`` raise, Ctrl-C during registration) can skip
      teardown;
    - ``resume(facets)`` is the single enable-then-resume gate, fed the facet
      list the ritual resolves (a settlement-active boot hosts no backrun
      lane; a backrun-only boot hosts the active arms and no registration);
    - registration is ONE scheduled hand-off — the scheduler is data (the
      production default is ``asyncio.create_task``) — and a backrun-only
      boot schedules nothing and never trims.
    """

    def __init__(self, host: BotRunner) -> None:
        self._host = host
        self._state = _RitualState.ATTACH_CONSUMER

    # ── The transition table ──────────────────────────────────────────

    def _expect(self, expected: _RitualState) -> None:
        """Admit only the transition whose state is current."""
        if self._state is not expected:
            msg = f"run ritual move {expected.value} refused: the ritual is in {self._state.value}"
            raise RunRitualError(msg)

    def attach_consumer(self) -> None:
        """Acquire the once-only block stream and start the drain task.

        The block-clock pipe is coordinator-owned: ``bot.block_stream()``
        moves the mpsc receiver out of the PumpState on each call — a second
        call raises, so the stream is acquired exactly once and fed DIRECTLY
        to the single result consumer (the Rust two-step gate + solve-time
        verifier own verification; no Python whole-batch re-verify).
        """
        self._expect(_RitualState.ATTACH_CONSUMER)
        host = self._host
        session = host.session
        assert session is not None
        assert session.bot is not None
        consumer = host.consumer or consume_result_batches
        block_stream = session.bot.block_stream()
        session.result_consumer_task = asyncio.create_task(
            consumer(session=session, block_stream=block_stream),
            name="result-consumer",
        )
        self._state = _RitualState.ATTACH_WATCH

    def attach_watch(self) -> None:
        """Attach the consumer to the session watch the moment it exists."""
        self._expect(_RitualState.ATTACH_WATCH)
        host = self._host
        session = host.session
        assert session is not None
        consumer_task = session.result_consumer_task
        assert consumer_task is not None
        host.session_watch.attach(
            consumer_task=consumer_task,
            watchdog_factory=host.pump_finished_watchdog,
        )
        self._state = _RitualState.RESUME

    def resume(self) -> None:
        """Resolve the hosted-arm facet set and resume the pump.

        The enabled-facet set is a data read over the same readiness view the
        boot gate consumed. ``resume()`` owns enabling each facet before it
        drives a hosted loop, so the enable-then-resume ordering lives in the
        engine, not here. A settlement-active boot is this runner's settlement
        arm, so it hosts no backrun lane.
        """
        self._expect(_RitualState.RESUME)
        host = self._host
        facets: list[str] = []
        if not host.settlement_active:
            view = host.readiness
            assert view is not None, "start() resolved the readiness before run()"
            facets = list(view.active_backrun_facets)
            bot_logger.info(
                f"[host-arms] settlement facet inactive — hosted arms: "
                f"{', '.join(facets) if facets else 'NONE'}"
            )
        session = host.session
        assert session is not None
        session.engine_registry.engine.resume(facets=facets)
        self._state = _RitualState.REGISTRATION

    def registration(self) -> None:
        """Schedule the registration hand-off — or skip it entirely.

        A backrun-only boot: the settlement pipeline (discovery ->
        registration -> sims -> submit arm) is the settlement facet's seam,
        and nothing else consumes registered paths, so registration does not
        run and the python-state trim never fires. The pump stays live: the
        hosted drivers it feeds read the head lanes and the reconcile guard,
        not registered paths.
        """
        self._expect(_RitualState.REGISTRATION)
        host = self._host
        if not host.settlement_active:
            self._state = _RitualState.MAIN_LOOP
            return
        registration_context, pipeline = self._build_registration_surface()
        task = host.scheduler(
            self._run_registration_background(
                path_builder=host.path_builder or build_paths,
                registration_context=registration_context,
                retry_policy=host.cfg.verification_retry_policy,
                pipeline=pipeline,
            )
        )
        # The scheduler seam carries no name, but the registration background
        # task has carried this observable name since HEAD (the deterministic
        # test double names its replay task the same way).
        task.set_name("registration-background")
        session = host.session
        assert session is not None
        session.registration_task = task
        # The optional registration member joins the watch-set.
        host.session_watch.attach_registration(task)
        self._state = _RitualState.MAIN_LOOP

    async def main_loop(self) -> SessionEndVerdict:
        """Watch the session's task set until it ends; re-raise a fatal registration.

        No startup batch verify precedes this loop — redundant with the
        per-pool two-step verify AND racy at the moving head: a block's
        header can advance ``last_processed_block()`` past a block before its
        Mint log is dispatched (V2-V2-V3 crash at mainnet 25397049). The
        per-pool gates are race-free (frozen-block pin); in-loop drift
        detection stays solver-side. The analyzer keys ``verify_basis`` on
        the per-pool ``[verify-seed]``/``[verify-drain]`` lines.
        """
        self._expect(_RitualState.MAIN_LOOP)
        host = self._host
        try:
            verdict = await host.session_watch.wait()
        finally:
            # Main loop ended while registration still climbs (shutdown):
            # the watch stops the dangling background task.
            await host.session_watch.teardown_registration()
        self._state = _RitualState.ENDED
        if verdict is SessionEndVerdict.RegistrationFailed:
            error = host.session_watch.registration_error
            assert error is not None
            raise error
        return verdict

    async def run(self) -> SessionEndVerdict:
        """Drive the five transitions in order — the ritual's only legal driver."""
        self.attach_consumer()
        self.attach_watch()
        self.resume()
        self.registration()
        return await self.main_loop()

    # ── The registration hand-off ─────────────────────────────────────

    def _build_registration_surface(self) -> tuple[ConstructionContext | None, Any]:
        """Build the construction context + pipeline for the REAL builder only.

        For the real ``build_paths``, the construction context is built ONCE
        here so the registration task owns it — a separate identity from the
        main-loop state that the trim (``release_python_state()`` + a dropped
        bot ref) never severs. The long-lived :class:`PathRegistrationPipeline`
        attaches to the session owner so the operator add-a-path surface
        (``enqueue_path`` / ``trigger_discovery``) stays reachable for the
        session's lifetime — including after the trim (the pipeline's retained
        context keeps constructing through the Rust ``PoolBuilder``).
        Injected builders (tests) skip context construction (fakes lack the
        builder surface) and receive ``context=None``.
        """
        host = self._host
        if host.path_builder is not None:
            return None, None
        session = host.session
        assert session is not None
        assert session.bot is not None
        registration_context = ConstructionContext.for_bot(session.bot, host.v3_snapshot)
        session.registration_context = registration_context
        pipeline = PathRegistrationPipeline(
            context=registration_context,
            engine_registry=session.engine_registry,
            retry_policy=host.cfg.verification_retry_policy,
            max_paths=host.cfg.max_registered_paths,
            discovery_batch_size=host.cfg.discovery_batch_size,
            progress_interval_secs=host.cfg.reg_progress_secs,
        )
        session.attach_registration_pipeline(pipeline)
        return registration_context, pipeline

    async def _run_registration_background(
        self,
        *,
        path_builder: Callable[..., Awaitable[None]],
        registration_context: ConstructionContext | None,
        retry_policy: RetryPolicy | None,
        pipeline: Any = None,
    ) -> None:
        """Run ``build_paths`` + the post-completion trim as the scheduled hand-off.

        Production decoupling: the hand-off coroutine is scheduled through the
        host's scheduler (the production default ``asyncio.create_task``) so
        the main loop starts before discovery completes. After the builder
        returns, the state-trim runs HERE as the coroutine's own completion
        duty, which the trim guard admits from this coroutine's task while
        refusing an outside mid-climb caller. A fatal verification error
        propagates out of ``build_paths`` and is surfaced by the fail-fast
        channel (the watch's ``RegistrationFailed`` verdict).

        Cooperative concurrency note: this coroutine runs on the asyncio loop,
        so it interleaves with the consumer only at ``await`` points
        (synchronous ``build_pool`` FFI calls still briefly occupy the loop
        thread). The pump itself solves on its own tokio thread regardless.
        """
        host = self._host
        try:
            await path_builder(
                bot=host.bot,
                engine_registry=host.engine_registry,
                options=BuildPathsOptions(
                    max_registered_paths=host.cfg.max_registered_paths,
                    discovery_batch_size=host.cfg.discovery_batch_size,
                    v3_snapshot=host.v3_snapshot,
                    v4_snapshot=host.v4_snapshot,
                    retry_policy=retry_policy,
                    context=registration_context,
                    pipeline=pipeline,
                    permutation_filter=host.cfg.permutation_filter,
                ),
            )
            host.trim()
        except asyncio.CancelledError:
            # Registration is being torn down mid-flight (cancelled by the
            # main loop's finally / a Ctrl-C / a fatal sim trap) BEFORE
            # `build_paths` finished. Registration offloads
            # `assemble_*_tick_map` (which clone the `Arc<SnapshotDb>`) onto a
            # ThreadPoolExecutor; `path_builder`'s futures are NOT
            # awaited/joined here, so worker threads may still be mid-
            # `assemble` holding their clones. Running `close_snapshot_tx()`
            # now would make the `Arc::try_unwrap` canary false-positive with
            # a secondary RuntimeError that masks the real teardown reason.
            # The WAL snapshot is a process-lifetime concern that becomes moot
            # at exit, so skip the read-tx commit/canary and let the
            # `Arc<SnapshotDb>` drop naturally with `Bot` — while the REST of
            # the state trim still runs. The healthy path below keeps the
            # canary fully active.
            host.trim(close_read_tx=False)
            raise
