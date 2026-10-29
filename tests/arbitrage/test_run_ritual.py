"""The run ritual's transition table, pinned against a real session.

``_run_ritual`` is the cockpit's ONE owner of a running session's startup
ordering (GLOSSARY.md *Run ritual*): consumer-attach, watch-attach, resume,
registration, main loop — an enum state machine whose transitions refuse to
run out of order (:class:`RunRitualError`, an internal-sequencing error
distinct from the operator-facing ``PhaseError``).

The host is a :class:`~degenbot.runner.session.Session` built through the
cockpit's one construction seam, so the ritual drives the SAME record and
mutators a boot does. These tests pin: every legal transition; the
``RunRitualError`` refusals; scheduler-as-data (exactly one task scheduled when
registration runs, zero when the arm skips it — and no trim); the consumer
attached before ``resume()`` as an observable fact; the registration failure
surfacing through the session watch's ``RegistrationFailed`` verdict and
re-raising in the caller's frame.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from degenbot.runner._run_ritual import RunRitual, RunRitualError, _RitualState
from degenbot.runner._session_watch import SessionEndVerdict, SessionWatch
from degenbot.runner.session import InjectedActors
from tests.deadlock_guard import DEADLOCK_GUARD_S
from tests.fakes.bot import FakeBot, RecordingPyBot
from tests.fakes.engine import FakeEngineRegistry
from tests.helpers.boot_actors import inline_registration_scheduler, noop_coro
from tests.helpers.session import OFFLINE_RELAY, FakeReadiness, session_for_test


class _RecordingWatch(SessionWatch):
    """The session watch, recording every ritual touch (the ordering facts)."""

    def __init__(self, events: list[str]) -> None:
        super().__init__()
        self.events = events
        self.attached_consumer: object | None = None
        self.attached_registration: object | None = None

    def attach(self, *, consumer_task: Any, watchdog_factory: Any) -> None:
        super().attach(consumer_task=consumer_task, watchdog_factory=watchdog_factory)
        self.events.append("watch-attach")
        self.attached_consumer = consumer_task

    def attach_registration(self, registration_task: Any) -> None:
        super().attach_registration(registration_task)
        self.events.append("watch-attach-registration")
        self.attached_registration = registration_task

    async def wait(self) -> SessionEndVerdict:
        self.events.append("main-loop")
        return await super().wait()

    async def teardown_registration(self) -> None:
        self.events.append("teardown-registration")
        await super().teardown_registration()


class _StubScheduler:
    """Records scheduled coroutines without driving them; returns a real Task.

    The seam type promises a coroutine->Task factory, so the stub returns a real
    ``asyncio.Task`` (a completed no-op) rather than the bare coroutine; the
    RECORDED coroutine itself is never driven (the stub watch never awaits it).
    """

    def __init__(self) -> None:
        self.scheduled: list[Any] = []
        self.tasks: list[asyncio.Task[Any]] = []

    def __call__(self, coro: Any) -> asyncio.Task[Any]:
        self.scheduled.append(coro)
        coro.close()  # never driven: the SCHEDULED COUNT is the fact under test
        task = asyncio.create_task(asyncio.sleep(0))
        self.tasks.append(task)
        return task


class _RecordingConsumer:
    """A consumer double that records its attach and returns a no-op coroutine."""

    def __init__(self, events: list[str]) -> None:
        self._events = events

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        self._events.append("consumer")
        return noop_coro()


class _RecordingBuilder:
    """A fake path builder: records its kwargs, completes without suspending."""

    def __init__(self, events: list[str]) -> None:
        self._events = events
        self.kwargs: dict[str, Any] | None = None

    async def __call__(self, **kwargs: Any) -> None:
        self._events.append("path-builder")
        self.kwargs = kwargs


def _session(
    events: list[str] | None = None,
    *,
    bot: Any = None,
    registry: Any = None,
    consumer: Any = None,
    builder: Any = None,
    scheduler: Any = None,
    settlement_active: bool = True,
    readiness: Any = None,
    watch: SessionWatch | None = None,
) -> Any:
    event_log = events if events is not None else []
    return session_for_test(
        bot=bot if bot is not None else FakeBot(events=event_log),
        engine_registry=registry if registry is not None else FakeEngineRegistry(events=event_log),
        consumer=consumer if consumer is not None else _RecordingConsumer(event_log),
        path_builder=builder if builder is not None else _RecordingBuilder(event_log),
        scheduler=scheduler if scheduler is not None else inline_registration_scheduler,
        settlement_active=settlement_active,
        readiness=readiness if readiness is not None else FakeReadiness(),
        session_watch=watch,
        relay_posture=OFFLINE_RELAY,
    )


def _hanging_consumer(events: list[str]) -> Any:
    async def _c(*args: Any, **kwargs: Any) -> None:
        events.append("consumer")
        await asyncio.Event().wait()

    return _c


class TestTransitionTable:
    async def test_legal_transitions_advance_the_state_machine(self) -> None:
        session = _session()
        ritual = RunRitual(session)
        assert ritual._state is _RitualState.ATTACH_CONSUMER

        await ritual.attach_consumer()
        assert ritual._state is _RitualState.ATTACH_WATCH
        assert session.result_consumer_task is not None
        ritual.attach_watch()
        assert ritual._state is _RitualState.RESUME
        ritual.resume()
        assert ritual._state is _RitualState.REGISTRATION
        ritual.registration()
        assert ritual._state is _RitualState.MAIN_LOOP
        verdict = await ritual.main_loop()

        assert verdict is SessionEndVerdict.PumpEnded
        assert ritual._state is _RitualState.ENDED

    async def test_run_drives_every_transition_in_order(self) -> None:
        events: list[str] = []
        watch = _RecordingWatch(events)
        session = _session(events, watch=watch)

        verdict = await RunRitual(session).run()

        assert verdict is SessionEndVerdict.PumpEnded
        # The deterministic scheduler drives the hand-off inline, so the
        # builder + the trim it completes land before the watch learns the
        # registration task; the startup ordering around them is what the
        # sequence pins.
        assert events == [
            "consumer",
            "watch-attach",
            "resume",
            "path-builder",
            "release",
            "watch-attach-registration",
            "main-loop",
            "teardown-registration",
        ]

    async def test_consumer_is_attached_before_resume(self) -> None:
        """The consumer task exists AND the watch holds it before any resume move."""
        events: list[str] = []
        watch = _RecordingWatch(events)
        session = _session(events, watch=watch)
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()

        assert watch.attached_consumer is session.result_consumer_task
        assert session.actors is not None
        assert session.actors.engine_registry.engine.resumed is False
        ritual.resume()
        assert events.index("consumer") < events.index("resume")
        assert events.index("watch-attach") < events.index("resume")


class TestRefusals:
    async def test_transitions_refused_before_the_ritual_started(self) -> None:
        for name in ("attach_watch", "resume", "registration"):
            ritual = RunRitual(_session())
            with pytest.raises(RunRitualError, match="refused"):
                getattr(ritual, name)()
        ritual = RunRitual(_session())
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    async def test_transitions_refused_mid_sequence(self) -> None:
        ritual = RunRitual(_session())
        await ritual.attach_consumer()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.attach_consumer()  # no re-entry
        with pytest.raises(RunRitualError, match="refused"):
            ritual.registration()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    async def test_an_ended_ritual_refuses_every_move(self) -> None:
        ritual = RunRitual(_session())
        await ritual.run()
        for name in ("attach_watch", "resume", "registration"):
            with pytest.raises(RunRitualError, match="refused"):
                getattr(ritual, name)()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.attach_consumer()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    def test_run_ritual_error_is_a_runtime_error(self) -> None:
        assert issubclass(RunRitualError, RuntimeError)


class TestSchedulerAsData:
    def test_injected_actors_default_scheduler_is_production(self) -> None:
        assert InjectedActors().scheduler is asyncio.create_task

    async def test_registration_runs_schedules_exactly_one_task(self) -> None:
        scheduler = _StubScheduler()
        events: list[str] = []
        watch = _RecordingWatch(events)
        session = _session(events, scheduler=scheduler, watch=watch)
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert len(scheduler.scheduled) == 1
        assert session.registration_task is scheduler.tasks[0]
        assert watch.attached_registration is scheduler.tasks[0]

    async def test_production_scheduler_names_the_registration_task(self) -> None:
        """The production default scheduler cannot carry a task name through
        the seam, so the ritual restores the observable name the registration
        background task carries."""
        session = _session(scheduler=asyncio.create_task)
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        task = session.registration_task
        assert isinstance(task, asyncio.Task)
        assert task.get_name() == "registration-background"
        await task  # the fake builder completes without suspending: reap it

    async def test_skipped_registration_schedules_nothing_and_never_trims(self) -> None:
        scheduler = _StubScheduler()
        events: list[str] = []
        watch = _RecordingWatch(events)
        bot = FakeBot(events=events)
        session = _session(
            events,
            bot=bot,
            scheduler=scheduler,
            settlement_active=False,
            watch=watch,
        )
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert scheduler.scheduled == []
        assert session.registration_task is None
        assert session.registration_context is None
        assert "watch-attach-registration" not in events
        assert bot.released is False


class TestRegistrationHandoff:
    async def test_registration_hands_the_session_data_to_the_builder(self) -> None:
        builder = _RecordingBuilder([])
        session = _session(builder=builder)
        assert session.actors is not None
        bot = session.actors.bot
        engine_registry = session.actors.engine_registry
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert session.actors is not None
        # The hand-off ran inline (the deterministic scheduler), so read the
        # identities it was handed before the trim drops the bot ref.
        assert builder.kwargs is not None
        assert builder.kwargs["bot"] is bot
        assert builder.kwargs["engine_registry"] is engine_registry
        # An injected builder has no construction surface: context=None.
        assert builder.kwargs["options"].context is None

    async def test_trim_runs_inside_the_registration_handoff(self) -> None:
        bot = FakeBot()
        session = _session(bot=bot)
        ritual = RunRitual(session)
        await ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert bot.released is True
        assert session.actors is not None
        assert session.actors.bot is None

    async def test_backrun_only_boot_enables_the_active_hosted_arms(self) -> None:
        readiness = FakeReadiness(active_backrun_facets=["txpool_backrun"], settlement_active=False)
        session = _session(settlement_active=False, readiness=readiness)
        await RunRitual(session).run()
        assert session.actors is not None
        assert session.actors.engine_registry.engine.resume_facets == ["txpool_backrun"]

    async def test_settlement_active_boot_hosts_no_backrun_lane(self) -> None:
        session = _session(settlement_active=True)
        await RunRitual(session).run()
        assert session.actors is not None
        assert session.actors.engine_registry.engine.resume_facets == []

    async def test_cancelled_registration_skips_the_read_tx_canary(self) -> None:
        """The hand-off's CancelledError semantics: skip the read-tx canary,
        run the rest of the trim."""
        started = asyncio.Event()

        async def hanging_builder(**kwargs: Any) -> None:
            started.set()
            await asyncio.Event().wait()

        calls: list[str] = []
        bot = FakeBot(py_bot=RecordingPyBot(calls))
        session = _session(bot=bot)
        ritual = RunRitual(session)
        task = asyncio.create_task(
            ritual._run_registration_background(
                path_builder=hanging_builder,
                registration_context=None,
                retry_policy=None,
            )
        )
        # Deadlock guard only — not an ordering assertion: the hanging builder
        # sets `started` before parking, so the edge is the gate. The budget only
        # turns "registration never started" into a named failure.
        await asyncio.wait_for(started.wait(), timeout=DEADLOCK_GUARD_S)
        task.cancel()

        with pytest.raises(asyncio.CancelledError):
            await task
        # The read-tx canary was skipped; the rest of the trim still ran.
        assert bot.released is True
        assert "close_snapshot_tx" not in calls

    async def test_registration_failure_surfaces_through_the_watch_verdict(self) -> None:
        """A fatal registration error takes the watch's RegistrationFailed
        verdict and re-raises in the caller's frame."""
        boom = RuntimeError("registration failed")

        def raising_builder(**kwargs: Any) -> None:
            raise boom

        session = _session(
            consumer=_hanging_consumer([]),
            builder=raising_builder,
            scheduler=inline_registration_scheduler,
            watch=SessionWatch(),
        )

        with pytest.raises(RuntimeError, match="registration failed"):
            await RunRitual(session).run()
