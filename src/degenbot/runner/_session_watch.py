"""One session watch behind the cockpit's end-state.

The cockpit's single owner of a pump session's end-state (the GLOSSARY.md
*session watch* term): the watch-set assembly ({consumer} + optional
{registration, watchdog} — exactly the task sets the former twin await loops
``_await_main_loop_with_registration_fail_fast`` /
``_await_main_loop_with_pump_watchdog`` watched), the fact gather, and the
cancel/teardown duties the ``run()`` finally / ``__aexit__`` / twin-loop sites
hand-rolled.

The verdict and the raise slot are the core's **Session-end ranking** (the
GLOSSARY.md term): this watch translates its own task facts into the core's
stable names and folds them through :func:`degenbot.arbitrage.session_end_rank`
(the ``_ffi`` door over ``degenbot_bot::arb_engine::session_end::rank``) — it
never infers a verdict from which task happened to finish first. It then
RENDERS the ranked product: at most one primary failure is delivered, in the
one ``RaiseSlot`` the core named; the consumer's output is retained and never
raised again over a registration or session-end failure.

The watch coordinates plain ``asyncio.Task`` objects handed to it by
:class:`~degenbot.runner.BotRunner` and touches no engine surface of its own
— pure Python, ``degenbot.runner``-internal (ADR-019).
"""

from __future__ import annotations

import asyncio
import contextlib
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage import session_end_rank
from degenbot.runner._consume import pump_stopped_abort
from degenbot.utils.tasks import cancel_and_reap

if TYPE_CHECKING:
    from collections.abc import Callable, Coroutine


#: The cockpit's watchdog is the pump-completion surface: its completion IS the
#: core's ``PumpFinished`` detection fact (a factory result naming a cause wins
#: over this default).
_DEFAULT_DETECTION_CAUSE = "PumpFinished"


class SessionEndVerdict(Enum):
    """How a pump session's main loop ended — the core's ranked verdict.

    Constructed from the core's ranked verdict NAME
    (``SessionEndVerdict(verdict_name)``), never from a local task-completion
    table. The values are the stable spellings both driver shells exchange:

    - ``PumpEnded``: the consumer ended the session (its intended close, or its
      own failure — the failure is delivered at the consumer raise slot).
    - ``RegistrationFailed``: a fatal registration error ended the session —
      the maximal fact; the error is stored on the watch
      (:attr:`SessionWatch.registration_error`) for the caller to re-raise.
    - ``WatchdogTripped``: the session ended outside its intended close —
      detection fired, or the consumer's stream stopped.
    """

    PumpEnded = "PumpEnded"
    RegistrationFailed = "RegistrationFailed"
    WatchdogTripped = "WatchdogTripped"


class RaiseSlot(Enum):
    """The one slot a primary failure may be delivered in.

    The core's ranked raise-slot name:

    - ``Nothing``: nothing raises (the intended close, or the empty
      no-failure remainder).
    - ``Registration``: raise the registration failure only.
    - ``Consumer``: raise the consumer's own failure only.
    - ``SessionEnd``: raise the unexpected-session-end failure only (the loud
      abort a supervised session must not survive): the ``pump-stopped`` end,
      or a detection fact without an intended consumer end.
    """

    Nothing = "Nothing"
    Registration = "Registration"
    Consumer = "Consumer"
    SessionEnd = "SessionEnd"


@dataclass(frozen=True)
class _WatchSet:
    """The session watch's typed task set: consumer + watchdog + optional registration.

    The set is the gather surface: a clean registration completion drops that
    member and keeps watching ``{consumer, watchdog}``; the verdict itself is
    never decided here — the core's ranking fold owns it.
    """

    consumer: asyncio.Task[Any]
    watchdog: asyncio.Task[Any]
    registration: asyncio.Task[Any] | None = None

    def members(self) -> set[asyncio.Task[Any]]:
        """Collect the tasks to wait on this batch.

        Returns:
            The still-watchable member tasks (registration drops out once
            it finishes cleanly).

        """
        members: set[asyncio.Task[Any]] = {self.consumer, self.watchdog}
        if self.registration is not None:
            members.add(self.registration)
        return members

    def without_registration(self) -> _WatchSet:
        """Drop the registration member from the watch-set.

        Returns:
            The watch-set over ``{consumer, watchdog}``.

        """
        return _WatchSet(self.consumer, self.watchdog, None)


def _watchdog_cause(watchdog_task: asyncio.Task[Any]) -> str:
    """Resolve the detection fact behind one completed watchdog.

    Its completion IS the pump-end detection fact; ``result()`` also surfaces a
    raising watchdog future rather than swallowing it. A factory result naming
    a cause wins over the pump-completion default.

    Args:
        watchdog_task: The completed watchdog task.

    Returns:
        The core detection-cause name.

    """
    result = watchdog_task.result()
    if isinstance(result, str) and result:
        return result
    return _DEFAULT_DETECTION_CAUSE


def _registration_fact(
    registration_task: asyncio.Task[Any],
) -> tuple[str | None, BaseException | None]:
    """Retrieve one registration task's outcome and translate it.

    A clean completion is the only ``Completed``: a raised error is a
    registration failure, and a cancelled task is teardown noise (not a fact).

    Args:
        registration_task: The finished registration task.

    Returns:
        The ``(outcome name, failure)`` pair; the outcome is ``None`` for
        teardown noise.

    """
    if registration_task.cancelled():
        return None, None
    error = registration_task.exception()
    if error is not None:
        return "Failed", error
    return "Completed", None


def _consumer_fact(
    consumer_task: asyncio.Task[Any],
) -> tuple[str, Any, BaseException | None]:
    """Retrieve one consumer task's output and translate its fate.

    The output is retrieved on EVERY path (so a simultaneous failure never
    leaves an unretrieved task exception); a returned report's ``end`` verdict
    translates through the exhaustive ``ConsumerEnd`` table (``quiet-end`` ->
    ``QuietEnd``, ``pump-stopped`` -> ``PumpStopped``), and a raised or
    cancelled task is the consumer failure.

    An ``end`` outside the table is refused LOUDLY here, before the core's
    Session-end ranking could fold an unknown ending into the intended close
    (``QuietEnd`` / ``PumpEnded`` + ``Nothing``).

    Args:
        consumer_task: The finished consumer task.

    Returns:
        The ``(outcome name, retained output, failure)`` triple.

    Raises:
        ValueError: when a returned report's ``end`` is not a ``ConsumerEnd``
            spelling; the refusal fires before the ranking fold.

    """
    if consumer_task.cancelled():
        return "Failed", None, asyncio.CancelledError()
    error = consumer_task.exception()
    if error is not None:
        return "Failed", None, error
    output = consumer_task.result()
    end = output.get("end") if isinstance(output, dict) else None
    match end:
        case "quiet-end":
            return "QuietEnd", output, None
        case "pump-stopped":
            return "PumpStopped", output, None
        case _:
            message = (
                f"unknown consumer end {end!r}: expected the ConsumerEnd spelling "
                "'quiet-end' or 'pump-stopped'"
            )
            raise ValueError(message)


def _session_end_abort(cause: str | None, consumer_outcome: str | None) -> RuntimeError:
    """Render the unexpected-session-end failure (the ``SessionEnd`` slot).

    The message names the ranked facts truthfully: the ``pump-stopped`` end
    keeps its operator-facing abort (:func:`pump_stopped_abort`), a detection
    fact without an intended consumer end names the cause that fired, and the
    empty remainder (no consumer end, no detection fact) says exactly that.

    Args:
        cause: The gathered detection-cause name (``None`` when detection
            never fired).
        consumer_outcome: The translated consumer outcome the fold ranked.

    Returns:
        The loud abort to raise.

    """
    if consumer_outcome == "PumpStopped":
        return pump_stopped_abort()
    if cause is not None:
        message = (
            f"the pump session ended unexpectedly: {cause} fired without an intended consumer end"
        )
        return RuntimeError(message)
    message = "the pump session ended unexpectedly: no consumer end and no detection fact"
    return RuntimeError(message)


class SessionWatch:
    """One owner of a pump session's end-state AND its task set.

    Watch-set, gather, teardown: task membership, cancellation, and reaping
    have this one home — the run ritual hands scheduled tasks straight here,
    and coordination reads the membership below rather than a mirrored task
    field.

    Watch-set: {consumer} + {pump-finished watchdog} + optional {background
    registration} — assembled exactly as the former twin loops' sets. The
    watchdog stays in the set for the WHOLE loop (a timed-exit pump can still
    finish after registration completes and must not be missed). Its
    completion IS a pump end, always: the engine's completion future stays
    pending until the pump genuinely stops, so test doubles that never finish
    simply park the watchdog.

    The verdict comes from the core's Session-end ranking over the gathered
    facts — never from which task finished first.
    """

    def __init__(self) -> None:
        self._consumer_task: asyncio.Task[Any] | None = None
        self._registration_task: asyncio.Task[Any] | None = None
        self._watchdog_factory: Callable[[], Coroutine[Any, Any, Any]] | None = None
        self._watchdog_task: asyncio.Task[Any] | None = None
        self._registration_error: BaseException | None = None
        self._consumer_output: Any = None

    # ── Watch-set assembly ────────────────────────────────────────────
    def attach(
        self,
        *,
        consumer_task: asyncio.Task[Any],
        watchdog_factory: Callable[[], Coroutine[Any, Any, Any]],
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

    # ── The narrow watch interface ───────────────────────────────────
    @property
    def consumer_task(self) -> asyncio.Task[Any] | None:
        """The watch-set's consumer member (``None`` before :meth:`attach`).

        The coordination read that replaces a mirrored task field: membership
        has one home, and cancellation/reaping stay the watch's duties.
        """
        return self._consumer_task

    @property
    def registration_task(self) -> asyncio.Task[Any] | None:
        """The watch-set's registration member (``None`` when the arm skipped it).

        Same one-home read as :attr:`consumer_task`; the trim guard compares
        task identity against it.
        """
        return self._registration_task

    @property
    def registration_error(self) -> BaseException | None:
        """The fatal registration error behind the ``RegistrationFailed`` verdict.

        ``None`` until then.
        """
        return self._registration_error

    @property
    def consumer_output(self) -> Any:
        """The consumer's retained output.

        Retrieved on every path; never re-raised over a registration or
        session-end failure.
        """
        return self._consumer_output

    # ── The gather ────────────────────────────────────────────────────
    async def wait(self) -> SessionEndVerdict:
        """Gather the session's end facts and render the core's ranked product.

        The gather is order-free: whichever member resolves first, the fold
        reads the gathered CONTENTS (a same-batch registration failure and
        consumer completion are both collected), so the verdict cannot depend
        on task completion order. The consumer's output is retrieved on every
        path and retained; a report whose ``end`` is not a ``ConsumerEnd``
        spelling refuses loudly at the translation, before the fold.

        Returns:
            The ranked end-state verdict — ``RegistrationFailed`` (the caller
            re-raises :attr:`registration_error`) or ``PumpEnded``. The
            ``WatchdogTripped`` verdict IS the unexpected session end and
            always raises its loud abort at the one session-end slot; the
            consumer's own failure raises at the one consumer slot.

        """
        consumer_task = self._consumer_task
        assert consumer_task is not None
        watchdog_factory = self._watchdog_factory
        assert watchdog_factory is not None
        watchdog_task = asyncio.create_task(watchdog_factory(), name="pump-finished-watchdog")
        self._watchdog_task = watchdog_task
        try:
            cause, registration = await self._gather(consumer_task, watchdog_task)
            retrieved = await self._retrieve(consumer_task)
            consumer_outcome, self._consumer_output, consumer_error = retrieved
            return self._render(cause, registration, consumer_outcome, consumer_error)
        finally:
            if not watchdog_task.done():
                # A watchdog still pending at exit is cancelled + reaped;
                # the reap asserts its cancellation was observed. A done
                # watchdog was already consumed in the gather (its fault
                # surfaced via result()) — reaping it here would re-raise
                # or mask the in-flight exception.
                await cancel_and_reap(watchdog_task)

    async def _gather(
        self,
        consumer_task: asyncio.Task[Any],
        watchdog_task: asyncio.Task[Any],
    ) -> tuple[str | None, str | None]:
        """Gather the detection cause and the registration outcome.

        Whichever member resolves first, the same-batch facts are collected
        too, so the returned contents are complete and order-free.

        Args:
            consumer_task: The watch-set's consumer member.
            watchdog_task: The watch-set's pump-finished watchdog member.

        Returns:
            The ``(detection cause, registration outcome)`` names (``None``
            for an absent fact).

        """
        watch = _WatchSet(consumer_task, watchdog_task, self._registration_task)
        cause: str | None = None
        registration: str | None = None
        # The watchdog stays in the watch-set for the WHOLE loop — including
        # after registration completes (only the main loop remains then, but
        # a timed-exit pump can still finish, and must not be missed).
        while not consumer_task.done():
            done, _pending = await asyncio.wait(
                watch.members(),
                return_when=asyncio.FIRST_COMPLETED,
            )
            if watch.registration is not None and watch.registration in done:
                registration, error = _registration_fact(watch.registration)
                if registration == "Failed":
                    # Fail-fast: the gather ends here (registration failure
                    # is maximal — the fold confirms it).
                    self._registration_error = error
                    break
                # Registration finished cleanly; stop watching it, keep
                # blocking on {consumer, watchdog}.
                watch = watch.without_registration()
            if watchdog_task in done:
                # Its completion IS the pump-end detection fact.
                cause = _watchdog_cause(watchdog_task)
                break

        # The fold reads gathered CONTENTS, never arrival order: pick up every
        # fact that resolved in the same batch as the one that ended the
        # gather.
        if cause is None and watchdog_task.done():
            cause = _watchdog_cause(watchdog_task)
        pending = self._registration_task
        if registration is None and pending is not None and pending.done():
            registration, error = _registration_fact(pending)
            if registration == "Failed":
                self._registration_error = error
        if cause is not None and pending is not None and not pending.done():
            # The detection path cancels the still-climbing registration
            # (the teardown duty reaps it) — the former twin's watchdog-True
            # branch.
            pending.cancel()
        return cause, registration

    @staticmethod
    async def _retrieve(
        consumer_task: asyncio.Task[Any],
    ) -> tuple[str, Any, BaseException | None]:
        """Cancel/reap a silent consumer and retrieve the consumer's output.

        A consumer that never reported is cancelled and reaped (the
        cancellation is OBSERVED); the output is then retrieved on every path.

        Args:
            consumer_task: The watch-set's consumer member.

        Returns:
            The ``(outcome name, retained output, failure)`` triple.

        """
        if not consumer_task.done():
            await cancel_and_reap(consumer_task)
        return _consumer_fact(consumer_task)

    @staticmethod
    def _render(
        cause: str | None,
        registration: str | None,
        consumer_outcome: str | None,
        consumer_error: BaseException | None,
    ) -> SessionEndVerdict:
        """Fold the gathered facts through the core rank and deliver per slot.

        Args:
            cause: The gathered detection-cause name.
            registration: The gathered registration-outcome name.
            consumer_outcome: The translated consumer-outcome name.
            consumer_error: The consumer's retrieved failure, if any.

        Returns:
            The ranked end-state verdict. The consumer's own failure raises at
            the one consumer slot; the unexpected-session-end abort raises at
            the one session-end slot.

        """
        verdict_name, slot_name = session_end_rank(
            cause=cause,
            consumer=consumer_outcome,
            registration=registration,
        )
        verdict = SessionEndVerdict(verdict_name)
        slot = RaiseSlot(slot_name)
        if slot is RaiseSlot.Registration:
            # Deliver ONLY the registration failure (the caller re-raises
            # :attr:`registration_error`); the consumer output is retained,
            # never raised again over it.
            return verdict
        if slot is RaiseSlot.Consumer and consumer_error is not None:
            raise consumer_error
        if slot is RaiseSlot.SessionEnd:
            abort = _session_end_abort(cause, consumer_outcome)
            raise abort
        return verdict

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
        (a no-op once :meth:`wait`'s exit drained it), and the session's
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
