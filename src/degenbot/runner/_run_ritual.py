"""The run ritual: one owner of a running session's startup ordering.

GLOSSARY.md *Run ritual*: the cockpit's one owner of a running session's
startup ordering — consumer-attach, watch-attach, resume, registration, main
loop — as a state machine; the registration mode and the task scheduler are
data it consumes, never branches tests select. It sits strictly behind the
``run()`` phase gate and re-legislates no legality the Rust ``SessionPhase``
table already owns.

The host is the :class:`~degenbot.runner.session.Session` that owns the run's
state: the ritual reads its collaborators, hands the consumer and registration
tasks straight to the session watch (task membership has one home), and writes
the registration-owned construction context through the session's mutator.
Ritual states are private; diagnostics read facts, never states.
"""

from __future__ import annotations

import asyncio
from enum import Enum
from typing import TYPE_CHECKING

from degenbot.logging import logger as bot_logger
from degenbot.runner._consume import consume_result_batches
from degenbot.runner._session_watch import SessionEndVerdict
from degenbot.runner.build_paths import (
    BuildPathsOptions,
    ConstructionContext,
    build_paths,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from degenbot.arbitrage import RetryPolicy
    from degenbot.runner.session import Session


class RunRitualError(RuntimeError):
    """A run-ritual sequencing invariant was violated.

    Distinct from the operator-facing :class:`~degenbot.runner.session.PhaseError`:
    the phase gate (the Rust host's ``SessionPhase`` table) owns lifecycle
    legality, while this error fires only when the ritual's own transitions
    are driven out of order — an internal invariant, unreachable through any
    operator action.
    """


class _RitualState(Enum):
    """The ritual's private states; the order below is the transition table."""

    ATTACH_CONSUMER = "attach_consumer"
    RESUME = "resume"
    ATTACH_WATCH = "attach_watch"
    REGISTRATION = "registration"
    MAIN_LOOP = "main_loop"
    ENDED = "ended"


class RunRitual:
    """The startup-ordering state machine behind ``Session.run()``.

    One legal drive: ``attach_consumer -> resume -> attach_watch ->
    registration -> main_loop``. Each named transition admits only its own
    state and refuses every other (:class:`RunRitualError`), so the ordering
    the engine requires lives in the machine rather than in call-site prose:

    - the consumer is BUILT first: it takes the engine's one event hand-off, so
      the session's stream has an owner before production can start;
    - ``resume`` is the engine's start flow in its core-owned order — enable the
      facets, then the consumer's ``resume`` (the claim's one use: the driver's
      ``run``), then the hosted-I/O boundary — fed the facet list the ritual
      resolves (a settlement-active boot hosts no backrun lane; a backrun-only
      boot hosts the active arms and no registration);
    - the consumer task and the session watch attach the moment the pump is
      armed, so a later failure (an inline ``build_paths`` raise, Ctrl-C during
      registration) still reaches the consumer;
    - registration is ONE scheduled hand-off — the scheduler is data (the
      production default is ``asyncio.create_task``) — and a backrun-only
      boot schedules nothing and never trims.
    """

    def __init__(self, session: Session) -> None:
        """Bind the session the ritual drives.

        Args:
            session: The started session whose run-phase handles the ritual
                writes.

        """
        self._session = session
        self._state = _RitualState.ATTACH_CONSUMER

    # ── The transition table ──────────────────────────────────────────

    def _expect(self, expected: _RitualState) -> None:
        """Admit only the transition whose state is current.

        Args:
            expected: The state the caller's transition requires.

        Raises:
            RunRitualError: If the ritual is in any other state.

        """
        if self._state is not expected:
            msg = f"run ritual move {expected.value} refused: the ritual is in {self._state.value}"
            raise RunRitualError(msg)

    async def attach_consumer(self) -> None:
        """Build the sim/submit pipeline and the session's core event consumer.

        The pipeline comes first (a refusal — no sim context, non-Alloy provider
        — aborts the run before the pump resumes), then the consumer takes the
        engine's one event hand-off. Nothing is spawned yet: the consumer task
        starts once the pump is armed, so the stream's first event cannot race a
        loop that production has not started under.

        """
        self._expect(_RitualState.ATTACH_CONSUMER)
        session = self._session
        await session.build_pipeline()
        await session.build_consumer()
        self._state = _RitualState.RESUME

    async def resume(self) -> None:
        """Resolve the hosted-arm facet set and start the session's production.

        The enabled-facet set is a data read over the same readiness view the
        boot gate consumed. Three calls in the core's order: the facets enable
        through the host admission gate, the consumer's ``resume`` consumes the
        session's one consumer claim (the driver's ``run`` — production cannot
        start without an attached consumer), and the deferred hosted I/O
        launches once the pump is armed. A settlement-active boot is this
        session's settlement arm, so it hosts no backrun lane.

        """
        self._expect(_RitualState.RESUME)
        session = self._session
        facets: list[str] = []
        if not session.settlement_active:
            view = session.readiness
            assert view is not None, "start() resolved the readiness before run()"
            facets = list(view.active_backrun_facets)
            bot_logger.info(
                f"[host-arms] settlement facet inactive - hosted arms: "
                f"{', '.join(facets) if facets else 'NONE'}"
            )
        actors = session.actors
        assert actors is not None, "the run ritual drives a started session"
        consumer = session.consumer
        assert consumer is not None, "attach_consumer built the session consumer"
        engine = actors.engine_registry.engine
        engine.enable_facets(facets)
        await consumer.resume()
        engine.start_hosted_session()
        self._state = _RitualState.ATTACH_WATCH

    def attach_watch(self) -> None:
        """Start the consumer task and attach it to the session watch.

        The task's lifetime is the session's: the watch holds it for the whole
        main loop, and its retrieval cancels and reaps the consumer when the
        pump ends outside ``stop()``.

        """
        self._expect(_RitualState.ATTACH_WATCH)
        session = self._session
        actors = session.actors
        assert actors is not None, "the run ritual drives a started session"
        consumer = session.consumer
        assert consumer is not None, "attach_consumer built the session consumer"
        adapter = actors.consumer or consume_result_batches
        consumer_task = asyncio.create_task(
            adapter(session.block_loop(), consumer),
            name="result-consumer",
        )
        # The task goes straight to the watch: task membership has one home.
        session.session_watch.attach(
            consumer_task=consumer_task,
            watchdog_factory=session.pump_finished_watchdog,
        )
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
        session = self._session
        actors = session.actors
        assert actors is not None, "the run ritual drives a started session"
        if not session.settlement_active:
            self._state = _RitualState.MAIN_LOOP
            return
        registration_context = self._build_registration_surface()
        task = actors.scheduler(
            self._run_registration_background(
                path_builder=actors.path_builder or build_paths,
                registration_context=registration_context,
                retry_policy=session.cfg.verification_retry_policy,
            )
        )
        # The scheduler seam carries no name, so the ritual restores the
        # registration task's observable name here (the deterministic double's
        # replay task uses the same one).
        task.set_name("registration-background")
        session.attach_registration_context(registration_context)
        # The optional registration member joins the watch-set — the task goes
        # straight to the watch (task membership has one home).
        session.session_watch.attach_registration(task)
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

        Returns:
            The session-end verdict. A fatal registration error comes back as
            the ``RegistrationFailed`` verdict and is re-raised in this
            caller's frame.

        """
        self._expect(_RitualState.MAIN_LOOP)
        session = self._session
        try:
            verdict = await session.session_watch.wait()
        finally:
            # Main loop ended while registration still climbs (shutdown):
            # the watch stops the dangling background task.
            await session.session_watch.teardown_registration()
        self._state = _RitualState.ENDED
        if verdict is SessionEndVerdict.RegistrationFailed:
            error = session.session_watch.registration_error
            assert error is not None
            raise error
        return verdict

    async def run(self) -> SessionEndVerdict:
        """Drive the five transitions in order — the ritual's only legal driver.

        Returns:
            The session-end verdict from the main loop.

        """
        await self.attach_consumer()
        await self.resume()
        self.attach_watch()
        self.registration()
        return await self.main_loop()

    # ── The registration hand-off ─────────────────────────────────────

    def _build_registration_surface(self) -> ConstructionContext | None:
        """Build the construction context for the REAL builder only.

        For the real ``build_paths``, the construction context is built ONCE
        here so the registration task owns it — a separate identity from the
        main-loop state that the trim (``release_python_state()`` + a dropped
        bot ref) never severs. ``build_paths`` boots the engine's crawl and
        parks the session ON this context, so the operator add-a-path surface
        (``enqueue_path`` / ``trigger_discovery``) stays reachable for the
        session's lifetime — including after the trim.
        Injected builders (tests) skip context construction (fakes lack the
        builder surface) and receive ``context=None``.

        Returns:
            The construction context, or ``None`` for an injected builder.

        """
        session = self._session
        actors = session.actors
        assert actors is not None, "the run ritual drives a started session"
        if actors.path_builder is not build_paths:
            return None
        bot = actors.bot
        assert bot is not None, "registration has not trimmed the bot yet"
        return ConstructionContext.for_bot(bot)

    async def _run_registration_background(
        self,
        *,
        path_builder: Callable[..., Awaitable[None]],
        registration_context: ConstructionContext | None,
        retry_policy: RetryPolicy | None,
    ) -> None:
        """Run ``build_paths`` + the post-completion trim as the scheduled hand-off.

        Production decoupling: the hand-off coroutine is scheduled through the
        session's scheduler (the production default ``asyncio.create_task``) so
        the main loop starts before discovery completes. After the builder
        returns, the state-trim runs HERE as the coroutine's own completion
        duty, which the trim guard admits from this coroutine's task while
        refusing an outside mid-climb caller. A fatal verification error
        propagates out of ``build_paths`` and is surfaced by the fail-fast
        channel (the watch's ``RegistrationFailed`` verdict).

        Args:
            path_builder: The registration builder to drive.
            registration_context: The registration-owned construction context.
            retry_policy: The verification retry policy.

        Raises:
            asyncio.CancelledError: Re-raised after the partial trim when
                registration is torn down mid-flight (main-loop shutdown,
                Ctrl-C, or a fatal sim trap).

        Cooperative concurrency note: this coroutine runs on the asyncio loop,
        so it interleaves with the consumer only at ``await`` points
        (synchronous ``build_pool`` FFI calls still briefly occupy the loop
        thread). The pump itself solves on its own tokio thread regardless.

        """
        session = self._session
        actors = session.actors
        assert actors is not None, "the run ritual drives a started session"
        try:
            await path_builder(
                bot=actors.bot,
                engine_registry=actors.engine_registry,
                options=BuildPathsOptions(
                    max_registered_paths=session.cfg.max_registered_paths,
                    discovery_batch_size=session.cfg.discovery_batch_size,
                    progress_interval_secs=session.cfg.reg_progress_secs,
                    retry_policy=retry_policy,
                    context=registration_context,
                    permutation_filter=session.cfg.permutation_filter,
                ),
            )
            session.trim_python_state()
        except asyncio.CancelledError:
            # Registration is being torn down mid-flight (cancelled by the main
            # loop's finally / a Ctrl-C / a fatal sim trap) BEFORE `build_paths`
            # finished. The trim is the SAME teardown as the healthy path: the
            # snapshot read tx closes through the core's retained-tx guard, which
            # drains the tick-map assemblies still holding a handle before it
            # runs its canary — so the cancel branch cannot reproduce the
            # false-positive a bare `Arc::try_unwrap` would report here.
            session.trim_python_state()
            raise
