"""The run ritual's transition table, pinned against a stub host.

``_run_ritual`` is the cockpit's ONE owner of a running session's startup
ordering (CONTEXT.md *Run ritual*): consumer-attach, watch-attach, resume,
registration, main loop — an enum state machine whose transitions refuse to
run out of order (:class:`RunRitualError`, an internal-sequencing error
distinct from the operator-facing ``PhaseError``).

Driven against a stub host, these tests pin: every legal transition; the
``RunRitualError`` refusals; scheduler-as-data (exactly one task scheduled
when registration runs, zero when the arm skips it — and no trim); the
consumer attached before ``resume()`` as an observable fact; the registration
failure surfacing through the session watch's ``RegistrationFailed`` verdict
and re-raising in the caller's frame.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from typing import Any

import pytest

from degenbot.runner._run_ritual import RunRitual, RunRitualError, _RitualState
from degenbot.runner._session_watch import SessionEndVerdict, SessionWatch
from degenbot.runner.bot_runner import InjectedActors
from tests.helpers.boot_actors import inline_registration_scheduler


def _noop_coro():
    async def _n() -> None:
        pass

    return _n()


class _StubBot:
    def __init__(self, events: list) -> None:
        self._events = events

    def block_stream(self):
        self._events.append("block-stream")
        return iter(())  # never iterated: the stub consumer ignores it


class _StubEngine:
    def __init__(self, events: list) -> None:
        self._events = events
        self.resume_facets: list[str] | None = None

    def resume(self, facets: list[str]) -> None:
        self._events.append("resume")
        self.resume_facets = list(facets)


class _StubRegistry:
    def __init__(self, events: list) -> None:
        self.engine = _StubEngine(events)


class _StubSession:
    """The session-owner surface the ritual reads (bot + engine registry)."""

    def __init__(self, events: list) -> None:
        self.bot = _StubBot(events)
        self.engine_registry = _StubRegistry(events)
        self.attached_pipelines: list = []
        # The run-phase handles the ritual writes (the stub mirrors
        # the real _SessionState fields).
        self.result_consumer_task = None
        self.registration_task = None
        self.registration_context = None

    def attach_registration_pipeline(self, pipeline: object) -> None:
        self.attached_pipelines.append(pipeline)


class _StubWatch:
    """The session-watch surface, recording every ritual touch."""

    def __init__(self, events: list) -> None:
        self._events = events
        self.attached_consumer: object | None = None
        self.attached_registration: object | None = None
        self.registration_error: BaseException | None = None

    def attach(self, *, consumer_task: object, watchdog_factory: object) -> None:
        self._events.append("watch-attach")
        self.attached_consumer = consumer_task

    def attach_registration(self, task: object) -> None:
        self._events.append("watch-attach-registration")
        self.attached_registration = task

    async def wait(self) -> SessionEndVerdict:
        self._events.append("main-loop")
        return SessionEndVerdict.PumpEnded

    async def teardown_registration(self) -> None:
        self._events.append("teardown-registration")


class _StubScheduler:
    """Records scheduled coroutines without driving them; returns a real Task.

    The seam type promises a coroutine→Task factory, so the stub returns a
    real ``asyncio.Task`` (a completed no-op) rather than the bare
    coroutine; the RECORDED coroutine itself is never driven (the stub
    watch never awaits it).
    """

    def __init__(self) -> None:
        self.scheduled: list = []
        self.tasks: list[asyncio.Task] = []

    def __call__(self, coro):
        self.scheduled.append(coro)
        coro.close()  # never driven: the SCHEDULED COUNT is the fact under test
        task = asyncio.create_task(asyncio.sleep(0))
        self.tasks.append(task)
        return task


class _StubConsumer:
    def __init__(self, events: list) -> None:
        self._events = events

    def __call__(self, *, session: object, block_stream: object):
        self._events.append("consumer")
        return _noop_coro()


class _FakeBuilder:
    """A fake path builder: records its kwargs, completes without suspending."""

    def __init__(self, events: list) -> None:
        self._events = events
        self.kwargs: dict | None = None

    async def __call__(self, **kwargs):
        self._events.append("path-builder")
        self.kwargs = kwargs


class _StubHost:
    """The narrow BotRunner surface the run ritual consumes."""

    def __init__(
        self,
        *,
        settlement_active: bool = True,
        builder: Any = None,
        scheduler: Any = None,
    ) -> None:
        self.events: list = []
        self.session = _StubSession(self.events)
        self.session_watch = _StubWatch(self.events)
        self.consumer = _StubConsumer(self.events)
        self.readiness = SimpleNamespace(active_backrun_facets=["txpool_backrun"])
        self.settlement_active = settlement_active
        self.path_builder = builder if builder is not None else _FakeBuilder(self.events)
        self.scheduler = scheduler if scheduler is not None else _StubScheduler()
        self.cfg = SimpleNamespace(
            verification_retry_policy=None,
            max_registered_paths=10,
            discovery_batch_size=100,
            permutation_filter=None,
        )
        self.v3_snapshot = None
        self.v4_snapshot = None
        self.bot = self.session.bot
        self.engine_registry = self.session.engine_registry

    def trim(self, *, close_read_tx: bool = True) -> None:
        self.events.append(("trim", close_read_tx))

    def pump_finished_watchdog(self):
        return _noop_coro()


class TestTransitionTable:
    async def test_legal_transitions_advance_the_state_machine(self) -> None:
        host = _StubHost()
        ritual = RunRitual(host)
        assert ritual._state is _RitualState.ATTACH_CONSUMER

        ritual.attach_consumer()
        assert ritual._state is _RitualState.ATTACH_WATCH
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
        host = _StubHost()
        verdict = await RunRitual(host).run()

        assert verdict is SessionEndVerdict.PumpEnded
        assert host.events == [
            "block-stream",
            "consumer",
            "watch-attach",
            "resume",
            "watch-attach-registration",
            "main-loop",
            "teardown-registration",
        ]

    async def test_consumer_is_attached_before_resume(self) -> None:
        """The consumer task exists AND the watch holds it before any resume move."""
        host = _StubHost()
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()

        assert host.session_watch.attached_consumer is host.session.result_consumer_task
        ritual.resume()
        assert host.events.index("consumer") < host.events.index("resume")
        assert host.events.index("watch-attach") < host.events.index("resume")


class TestRefusals:
    async def test_transitions_refused_before_the_ritual_started(self) -> None:
        for name in ("attach_watch", "resume", "registration"):
            ritual = RunRitual(_StubHost())
            with pytest.raises(RunRitualError, match="refused"):
                getattr(ritual, name)()
        ritual = RunRitual(_StubHost())
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    async def test_transitions_refused_mid_sequence(self) -> None:
        ritual = RunRitual(_StubHost())
        ritual.attach_consumer()
        with pytest.raises(RunRitualError, match="refused"):
            ritual.attach_consumer()  # no re-entry
        with pytest.raises(RunRitualError, match="refused"):
            ritual.registration()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    async def test_an_ended_ritual_refuses_every_move(self) -> None:
        ritual = RunRitual(_StubHost())
        await ritual.run()
        for name in ("attach_consumer", "attach_watch", "resume", "registration"):
            with pytest.raises(RunRitualError, match="refused"):
                getattr(ritual, name)()
        with pytest.raises(RunRitualError, match="refused"):
            await ritual.main_loop()

    def test_run_ritual_error_is_a_runtime_error(self) -> None:
        assert issubclass(RunRitualError, RuntimeError)


class TestSchedulerAsData:
    def test_injected_actors_default_scheduler_is_production(self) -> None:
        assert InjectedActors().scheduler is asyncio.create_task

    async def test_registration_runs_schedules_exactly_one_task(self) -> None:
        host = _StubHost()
        scheduler = _StubScheduler()
        host.scheduler = scheduler
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert len(scheduler.scheduled) == 1
        assert host.session_watch.attached_registration is scheduler.tasks[0]

    async def test_production_scheduler_names_the_registration_task(self) -> None:
        """The production default scheduler cannot carry a task name through
        the seam, so the ritual restores the observable name HEAD's
        registration background task carried."""
        host = _StubHost(scheduler=asyncio.create_task)
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        task = host.session.registration_task
        assert isinstance(task, asyncio.Task)
        assert task.get_name() == "registration-background"
        await task  # the fake builder completes without suspending: reap it

    async def test_skipped_registration_schedules_nothing_and_never_trims(self) -> None:
        host = _StubHost(settlement_active=False)
        scheduler = _StubScheduler()
        host.scheduler = scheduler
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert scheduler.scheduled == []
        assert host.session_watch.attached_registration is None
        assert "watch-attach-registration" not in host.events
        assert not any(
            e == "trim" or (isinstance(e, tuple) and e[0] == "trim") for e in host.events
        )


class TestRegistrationHandoff:
    async def test_registration_hands_the_session_data_to_the_builder(self) -> None:
        host = _StubHost(scheduler=inline_registration_scheduler)
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        builder = host.path_builder
        assert builder.kwargs is not None
        assert builder.kwargs["bot"] is host.bot
        assert builder.kwargs["engine_registry"] is host.engine_registry
        # An injected builder has no construction surface: context=None.
        assert builder.kwargs["options"].context is None

    async def test_trim_runs_inside_the_registration_handoff(self) -> None:
        host = _StubHost(scheduler=inline_registration_scheduler)
        ritual = RunRitual(host)
        ritual.attach_consumer()
        ritual.attach_watch()
        ritual.resume()

        ritual.registration()

        assert ("trim", True) in host.events

    async def test_backrun_only_boot_enables_the_active_hosted_arms(self) -> None:
        host = _StubHost(settlement_active=False)
        await RunRitual(host).run()
        assert host.session.engine_registry.engine.resume_facets == ["txpool_backrun"]

    async def test_settlement_active_boot_hosts_no_backrun_lane(self) -> None:
        host = _StubHost(settlement_active=True)
        await RunRitual(host).run()
        assert host.session.engine_registry.engine.resume_facets == []

    async def test_cancelled_registration_skips_the_read_tx_canary(self) -> None:
        """The hand-off's CancelledError semantics: skip the read-tx canary,
        run the rest of the trim."""
        host = _StubHost()
        started = asyncio.Event()

        async def hanging_builder(**kwargs):
            started.set()
            await asyncio.Event().wait()

        host.path_builder = hanging_builder
        ritual = RunRitual(host)
        task = asyncio.create_task(
            ritual._run_registration_background(
                path_builder=hanging_builder,
                registration_context=None,
                retry_policy=None,
            )
        )
        await asyncio.wait_for(started.wait(), timeout=1)
        task.cancel()

        with pytest.raises(asyncio.CancelledError):
            await task
        assert ("trim", False) in host.events
        assert ("trim", True) not in host.events

    async def test_registration_failure_surfaces_through_the_watch_verdict(self) -> None:
        """A fatal registration error takes the watch's RegistrationFailed
        verdict and re-raises in the caller's frame."""
        boom = RuntimeError("registration failed")

        def raising_builder(**kwargs):
            raise boom

        async def hanging_consumer(**kwargs):
            await asyncio.Event().wait()

        async def hanging_watchdog():
            await asyncio.Event().wait()

        host = _StubHost(
            scheduler=inline_registration_scheduler,
            builder=raising_builder,
        )
        host.consumer = hanging_consumer
        host.pump_finished_watchdog = hanging_watchdog
        host.session_watch = SessionWatch()

        with pytest.raises(RuntimeError, match="registration failed"):
            await RunRitual(host).run()
