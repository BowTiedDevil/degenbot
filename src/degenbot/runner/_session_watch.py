"""One session watch behind the cockpit's end-state.

The cockpit's single owner of a pump session's end-state (the GLOSSARY.md
*session watch* term): the watch-set assembly ({consumer} + optional
{registration, watchdog} — exactly the task sets the former twin await loops
``_await_main_loop_with_registration_fail_fast`` /
``_await_main_loop_with_pump_watchdog`` watched), the
:class:`SessionEndVerdict` ranking (fail-fast registration outranks a
watchdog trip in the same wait batch — a property of the typed
:class:`_WatchSet`, not of the await loop), and the cancel/teardown duties the
``run()`` finally / ``__aexit__`` / twin-loop sites hand-rolled.

The watch coordinates plain ``asyncio.Task`` objects handed to it by
:class:`~degenbot.runner.BotRunner` and touches no engine surface of its own
— pure Python, ``degenbot.runner``-internal (ADR-019).
"""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass
from enum import Enum, auto
from typing import TYPE_CHECKING, Any

from degenbot.utils.tasks import cancel_and_reap

if TYPE_CHECKING:
    from collections.abc import Callable, Coroutine


class SessionEndVerdict(Enum):
    """How a pump session's main loop ended (the *session watch* verdict).

    - ``PumpEnded``: the consumer task itself ended (block/result streams
      closed, or the consumer raised — the exception propagates from
      :meth:`SessionWatch.wait` exactly as the former twins' plain
      ``await main_task`` did).
    - ``RegistrationFailed``: a fatal registration error was surfaced through
      the cross-task fail-fast channel; the consumer was cancelled and the
      error stored on the watch (:attr:`SessionWatch.registration_error`) for
      the caller to re-raise. In a same-batch race this verdict OUTRANKS
      ``WatchdogTripped``.
    - ``WatchdogTripped``: the pump-finished watchdog fired (the pump ended
      outside ``stop()``); it already cancelled the consumer, and the session
      leaves via the normal graceful teardown.
    """

    PumpEnded = auto()
    RegistrationFailed = auto()
    WatchdogTripped = auto()


class _WatchKind(Enum):
    """The typed per-batch decision of the session watch's task set."""

    CONTINUE = auto()
    REGISTRATION_FAILED = auto()
    WATCHDOG_TRIPPED = auto()


@dataclass(frozen=True)
class _WatchSet:
    """The session watch's typed task set: consumer + watchdog + optional registration.

    :meth:`on_task_done` owns the batched legality decision — a fatal
    registration error outranks a watchdog trip in the same wait batch — so the
    await loop only applies the transition it returns. A clean registration
    completion drops that member and keeps watching ``{consumer, watchdog}``.
    """

    consumer: asyncio.Task[Any]
    watchdog: asyncio.Task[None]
    registration: asyncio.Task[Any] | None = None

    def members(self) -> set[asyncio.Task[Any]]:
        """Collect the tasks to wait on this batch.

        Returns:
            The still-watchable member tasks (registration drops out once
            it finishes).

        """
        members: set[asyncio.Task[Any]] = {self.consumer, self.watchdog}
        if self.registration is not None:
            members.add(self.registration)
        return members

    def on_task_done(self, done: set[asyncio.Task[Any]]) -> _WatchTransition:
        """Decide the session's next move from one completed wait batch.

        Returns:
            The watch transition (next watch-set + verdict kind).

        """
        registration = self.registration
        if registration is not None and registration in done:
            exc = registration.exception()
            if exc is not None and not isinstance(exc, asyncio.CancelledError):
                return _WatchTransition(_WatchKind.REGISTRATION_FAILED, self, error=exc)
            # Registration finished cleanly; stop watching it, keep
            # blocking on {consumer, watchdog}.
            return _WatchTransition(
                _WatchKind.CONTINUE,
                _WatchSet(self.consumer, self.watchdog, None),
            )
        if self.watchdog in done:
            return _WatchTransition(_WatchKind.WATCHDOG_TRIPPED, self)
        return _WatchTransition(_WatchKind.CONTINUE, self)


@dataclass(frozen=True)
class _WatchTransition:
    """One :meth:`_WatchSet.on_task_done` decision: its kind, watch-set, and error."""

    kind: _WatchKind
    watch: _WatchSet
    error: BaseException | None = None


class SessionWatch:
    """One owner of a pump session's end-state: watch-set, verdict, teardown.

    Watch-set: {consumer} + {pump-finished watchdog} + optional {background
    registration} — assembled exactly as the former twin loops' sets. The
    watchdog stays in the set for the WHOLE loop (a timed-exit pump can still
    finish after registration completes and must not be missed). Its
    completion IS a pump end, always: the engine's completion future stays
    pending until the pump genuinely stops, so test doubles that never finish
    simply park the watchdog.
    """

    def __init__(self) -> None:
        self._consumer_task: asyncio.Task[Any] | None = None
        self._registration_task: asyncio.Task[Any] | None = None
        self._watchdog_factory: Callable[[], Coroutine[Any, Any, None]] | None = None
        self._watchdog_task: asyncio.Task[None] | None = None
        self._registration_error: BaseException | None = None

    # ── Watch-set assembly ────────────────────────────────────────────
    def attach(
        self,
        *,
        consumer_task: asyncio.Task[Any],
        watchdog_factory: Callable[[], Coroutine[Any, Any, None]],
    ) -> None:
        """Attach the always-on watch members.

        The members are the consumer task and the pump-finished watchdog factory.

        Called the moment the consumer task exists, so a teardown after any
        later ``run()`` failure (e.g. an inline ``build_paths`` raise) still
        reaches the consumer.
        """
        self._consumer_task = consumer_task
        self._watchdog_factory = watchdog_factory

    def attach_registration(self, registration_task: asyncio.Task[Any]) -> None:
        """Attach the optional registration member (the Sub-B background task).

        Inline/injected sessions never call this — the watch-set is
        then exactly {consumer, watchdog}.
        """
        self._registration_task = registration_task

    @property
    def registration_error(self) -> BaseException | None:
        """The fatal registration error behind the ``RegistrationFailed`` verdict.

        ``None`` until then.
        """
        return self._registration_error

    # ── The wait ──────────────────────────────────────────────────────
    async def wait(self) -> SessionEndVerdict:
        """Watch the session's task set until the session ends; return the verdict.

        The batched legality decision lives on the typed :class:`_WatchSet`
        (:meth:`_WatchSet.on_task_done`), not in this loop: a fail-fast
        registration verdict outranks a watchdog verdict in the same wait
        batch. On the fail-fast path the consumer is cancelled + drained and
        the error stored; the caller re-raises it.

        Returns:
            The end-state verdict — ``RegistrationFailed`` (caller must
            re-raise :attr:`registration_error`), ``WatchdogTripped``
            (graceful teardown; a raising watchdog future surfaces here via
            ``result()``), or ``PumpEnded`` (the session ran to its own end).

        """
        consumer_task = self._consumer_task
        assert consumer_task is not None
        watchdog_factory = self._watchdog_factory
        assert watchdog_factory is not None
        watchdog_task = asyncio.create_task(watchdog_factory(), name="pump-finished-watchdog")
        self._watchdog_task = watchdog_task
        watch = _WatchSet(consumer_task, watchdog_task, self._registration_task)
        pump_ended = False
        try:
            # The watchdog stays in the watch-set for the WHOLE loop — including
            # after registration completes (only the main loop remains then, but
            # a timed-exit pump can still finish, and must not be missed).
            while not consumer_task.done():
                done, _pending = await asyncio.wait(
                    watch.members(),
                    return_when=asyncio.FIRST_COMPLETED,
                )
                step = watch.on_task_done(done)
                watch = step.watch
                if step.kind is _WatchKind.REGISTRATION_FAILED:
                    # Fatal registration error → fail loudly: stop the hot
                    # loop; the reap asserts the cancellation was observed.
                    await cancel_and_reap(consumer_task)
                    self._registration_error = step.error
                    return SessionEndVerdict.RegistrationFailed
                if step.kind is _WatchKind.WATCHDOG_TRIPPED:
                    # The completion future resolves ONLY when the pump really
                    # stopped, so its completion IS a pump end; surface a
                    # watchdog fault (a raising future) rather than swallowing it.
                    watchdog_task.result()
                    registration = watch.registration
                    if registration is not None and not registration.done():
                        registration.cancel()
                    pump_ended = True
                    break
            if pump_ended:
                # No local cancel: the watchdog (or the pump's own stop)
                # already ended the consumer, so its unwind here is the
                # graceful end — an externally requested cancellation is
                # teardown noise, not a lost failure.
                with contextlib.suppress(asyncio.CancelledError):
                    await consumer_task
                return SessionEndVerdict.WatchdogTripped
            await consumer_task
            return SessionEndVerdict.PumpEnded
        finally:
            if not watchdog_task.done():
                # A watchdog still pending at exit is cancelled + reaped;
                # the reap asserts its cancellation was observed. A done
                # watchdog was already consumed in the loop (its fault
                # surfaced via result()) — reaping it here would re-raise
                # or mask the in-flight exception.
                await cancel_and_reap(watchdog_task)

    # ── Teardown ──────────────────────────────────────────────────────
    async def teardown_registration(self) -> None:
        """Drain/cancel the background registration task (the ``run()`` finally duty).

        Main loop ended while registration still climbs (shutdown): stop the
        dangling background task. Idempotent — a completed (or already
        cancelled) task is left alone.
        """
        registration_task = self._registration_task
        if (
            registration_task is not None
            and not registration_task.done()
            and not registration_task.cancelled()
        ):
            await cancel_and_reap(registration_task)

    async def teardown(self) -> None:
        """Tear down the session idempotently at end-of-session.

        Folds the cancel/teardown duties the ``run()`` finally and
        ``__aexit__`` hand-rolled: the registration drain (a no-op once
        :meth:`teardown_registration` ran from ``run()``), the watchdog reap
        (a no-op once ``wait()``'s exit drained it), and the session's
        consumer cancel — the latter must run AFTER the pump was stopped by
        ``shutdown()`` (see the ``__aexit__`` ordering rationale; that is why
        ``run()``'s finally calls the focused :meth:`teardown_registration`
        instead of this full teardown).
        """
        await self.teardown_registration()
        watchdog_task = self._watchdog_task
        if watchdog_task is not None and not watchdog_task.done():
            await cancel_and_reap(watchdog_task)
        consumer_task = self._consumer_task
        if consumer_task is not None and not consumer_task.done():
            # Best-effort, deliberately without the cancelled() assertion:
            # the consumer may unwind with a real error after the pump
            # stop, and teardown must never mask the original in-flight
            # exception (the __aexit__ contract).
            consumer_task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await consumer_task
