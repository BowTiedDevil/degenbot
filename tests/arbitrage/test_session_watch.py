"""The session watch's end-state matrix.

The cockpit's ONE owner of a pump session's end-state
(``degenbot.runner._session_watch`` — the GLOSSARY.md *session watch* term):
the watch-set assembly ({consumer} + optional {registration, watchdog},
exactly the task sets the former twin await loops watched), the fact gather
over the core's **Session-end ranking** (``session_end_rank`` — the verdict
and raise slot come from the core fold, never a local task-completion table),
and the idempotent teardown the ``run()`` finally / ``__aexit__`` /
twin-loop sites hand-rolled.

Pure asyncio — no engine, no RPC (ADR-019: the watch stays inside
``degenbot.runner`` and coordinates plain ``asyncio.Task`` objects).
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.arbitrage import session_end_rank
from degenbot.runner import _session_watch
from degenbot.runner._consume import _PUMP_STOPPED_MESSAGE
from degenbot.runner._session_watch import (
    RaiseSlot,
    SessionEndVerdict,
    SessionWatch,
    _session_end_abort,
    _WatchSet,
)

if TYPE_CHECKING:
    from collections.abc import Coroutine


class _RegistrationPanic(BaseException):
    """The Python analogue of a Rust background-registration task panic."""


async def _quick_consumer(ticks: int = 1) -> dict[str, Any]:
    """A consumer that ends on its own after a few loop ticks, reporting the
    quiet end in the typed report shape every consumer returns."""
    for _ in range(ticks):
        await asyncio.sleep(0)
    return {"batches": 0, "end": "quiet-end"}


async def _hanging_consumer() -> None:
    await asyncio.Event().wait()


def _never_watchdog() -> Coroutine[Any, Any, None]:
    """A watchdog whose pump never finishes (the pre-finish consumer shape)."""

    async def _watchdog() -> None:
        await asyncio.Event().wait()

    return _watchdog()


def _pump_end_watchdog() -> Coroutine[Any, Any, str]:
    """Production pump-finished watchdog semantics: resolve the detection
    fact and stop there. The WATCH owns consumer cancellation (its retrieval
    cancels and reaps an idling consumer), so the detection path cancels
    nothing itself."""

    async def _watchdog() -> str:
        await asyncio.sleep(0)
        return "PumpFinished"

    return _watchdog()


def _watch(
    consumer: asyncio.Task[Any],
    watchdog_factory: Any,
    registration: asyncio.Task[Any] | None = None,
) -> SessionWatch:
    watch = SessionWatch()
    watch.attach(consumer_task=consumer, watchdog_factory=watchdog_factory)
    if registration is not None:
        watch.attach_registration(registration)
    return watch


async def _cancellation_absorbing_consumer() -> None:
    """A task that absorbs its cancellation and ends un-cancelled."""
    try:
        await asyncio.Event().wait()
    except asyncio.CancelledError:
        return


def _absorbing_watchdog_factory() -> Coroutine[Any, Any, None]:
    """A watchdog that absorbs the reap's cancel instead of unwinding."""

    async def _watchdog() -> None:
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            return

    return _watchdog()


def _report_consumer(end: str | None) -> Any:
    """A consumer double returning the typed report shape (``end`` is the
    ``ConsumerEnd`` spelling the FFI report carries)."""

    async def _consumer() -> dict[str, Any]:
        await asyncio.sleep(0)
        return {"batches": 0, "end": end}

    return _consumer


class TestVerdictEnum:
    def test_verdict_members_pin_the_end_state(self) -> None:
        """The settled enum: exactly the three end-state members."""
        assert {verdict.name for verdict in SessionEndVerdict} == {
            "PumpEnded",
            "RegistrationFailed",
            "WatchdogTripped",
        }

    def test_verdict_and_slot_construct_from_core_output(self) -> None:
        """``SessionEndVerdict`` / ``RaiseSlot`` are constructed from the core
        rank's stable names — not from a local task-completion table."""
        assert SessionEndVerdict("PumpEnded") is SessionEndVerdict.PumpEnded
        assert SessionEndVerdict("RegistrationFailed") is SessionEndVerdict.RegistrationFailed
        assert SessionEndVerdict("WatchdogTripped") is SessionEndVerdict.WatchdogTripped
        assert RaiseSlot("Nothing") is RaiseSlot.Nothing
        assert RaiseSlot("Registration") is RaiseSlot.Registration
        assert RaiseSlot("Consumer") is RaiseSlot.Consumer
        assert RaiseSlot("SessionEnd") is RaiseSlot.SessionEnd


class TestCoreRankDoor:
    """The FFI door over the core ``rank`` fold, by stable names."""

    def test_the_rank_door_exposes_the_core_verdicts_and_slots(self) -> None:
        assert session_end_rank(cause=None, consumer="QuietEnd", registration=None) == (
            "PumpEnded",
            "Nothing",
        )
        assert session_end_rank(cause="PumpFinished", consumer="Failed", registration="Failed") == (
            "RegistrationFailed",
            "Registration",
        )
        assert session_end_rank(cause=None, consumer="PumpStopped", registration=None) == (
            "WatchdogTripped",
            "SessionEnd",
        )
        assert session_end_rank(cause="PumpFinished", consumer=None, registration=None) == (
            "WatchdogTripped",
            "SessionEnd",
        )
        assert session_end_rank(cause=None, consumer=None, registration=None) == (
            "PumpEnded",
            "Nothing",
        )

    def test_the_rank_door_refuses_names_outside_the_core_vocabulary(self) -> None:
        with pytest.raises(ValueError, match="session-end cause"):
            session_end_rank(cause="nope", consumer=None, registration=None)
        with pytest.raises(ValueError, match="consumer outcome"):
            session_end_rank(cause=None, consumer="nope", registration=None)
        with pytest.raises(ValueError, match="registration outcome"):
            session_end_rank(cause=None, consumer=None, registration="nope")


class TestWatchSetMatrix:
    async def test_consumer_alone_ends_pump_ended(self) -> None:
        """{consumer} + watchdog: the consumer ending on its own is a
        ``PumpEnded`` verdict, and the watchdog is assembled + drained."""
        consumer = asyncio.create_task(_quick_consumer())
        factory_calls = 0

        def factory() -> Coroutine[Any, Any, bool]:
            nonlocal factory_calls
            factory_calls += 1
            return _never_watchdog()

        watch = _watch(consumer, factory)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        assert consumer.done()
        assert not consumer.cancelled()
        # The watchdog was assembled into the watch-set (both former twins
        # always created it) and wait()'s exit drained it.
        assert factory_calls == 1
        watchdog_task = watch._watchdog_task
        assert watchdog_task is not None
        assert watchdog_task.done()
        assert watchdog_task.cancelled()

    async def test_consumer_exception_propagates_from_wait(self) -> None:
        """The consumer's own failure is delivered at the one consumer raise
        slot — the plain ``await main_task`` tail the twins kept."""
        boom = RuntimeError("loud abort")

        async def consumer() -> None:  # ruff: ignore[unused-async] -- a raising task double
            raise boom

        task = asyncio.create_task(consumer())
        watch = _watch(task, _never_watchdog)

        with pytest.raises(RuntimeError) as excinfo:
            await watch.wait()
        assert excinfo.value is boom

    async def test_registration_completing_cleanly_is_dropped(self) -> None:
        """A clean registration completion is a no-op: dropped from the
        watch-set, the loop keeps blocking on {consumer, watchdog}."""
        consumer = asyncio.create_task(_quick_consumer(ticks=5))

        async def clean_registration() -> None:
            await asyncio.sleep(0)

        registration = asyncio.create_task(clean_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        assert consumer.done()
        assert not consumer.cancelled()
        assert registration.done()
        assert registration.exception() is None
        assert watch.registration_error is None

    async def test_fatal_registration_fails_fast(self) -> None:
        """A fatal registration error cancels the main-loop consumer and is
        delivered as the ``RegistrationFailed`` verdict (error retrievable
        from the watch for the caller to re-raise)."""
        consumer = asyncio.create_task(_hanging_consumer())
        boom = ValueError("verification mismatch")

        async def failing_registration() -> None:
            await asyncio.sleep(0)
            raise boom

        registration = asyncio.create_task(failing_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        assert await watch.wait() is SessionEndVerdict.RegistrationFailed
        assert watch.registration_error is boom
        # Fail-fast cancelled the hot loop (and drained it).
        assert consumer.cancelled()
        assert registration.done()
        assert registration.exception() is boom

    async def test_same_batch_fail_fast_outranks_watchdog(self) -> None:
        """THE ranking, written once in the core: when a fatal registration
        error and a watchdog trip complete in the SAME wait batch, the
        registration verdict outranks the detection fact."""
        consumer = asyncio.create_task(_hanging_consumer())
        boom = ValueError("verification mismatch")

        async def failing_registration() -> None:  # ruff: ignore[unused-async] -- a raising task double
            raise boom

        registration = asyncio.create_task(failing_registration())

        def instant_finish_watchdog() -> Coroutine[Any, Any, None]:
            # Deliberately does NOT cancel the consumer: if this verdict won,
            # the consumer would be left pending and the verdict would be
            # WatchdogTripped — the assertion below pins which branch ran.
            async def _watchdog() -> None:  # ruff: ignore[unused-async] -- an instant task double
                return

            return _watchdog()

        watch = _watch(consumer, instant_finish_watchdog, registration)

        assert await watch.wait() is SessionEndVerdict.RegistrationFailed
        assert watch.registration_error is boom
        # The consumer was cancelled by the FAIL-FAST path, not left idle.
        assert consumer.cancelled()

    async def test_same_batch_registration_failure_retrieves_the_consumer_output(self) -> None:
        """Same-batch registration failure + consumer error: the verdict is
        ``RegistrationFailed``, the consumer's output is retrieved (never left
        unretrieved), and ONLY the registration failure is delivered."""
        boom = ValueError("consumer boom")
        reg_boom = ValueError("reg boom")

        async def failing_consumer() -> None:  # ruff: ignore[unused-async] -- a raising task double
            raise boom

        async def failing_registration() -> None:  # ruff: ignore[unused-async] -- a raising task double
            raise reg_boom

        consumer = asyncio.create_task(failing_consumer())
        registration = asyncio.create_task(failing_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        # The consumer's boom is retrieved and retained — NOT raised over the
        # registration failure (the no-double-raise rule).
        assert await watch.wait() is SessionEndVerdict.RegistrationFailed
        assert watch.registration_error is reg_boom
        assert consumer.exception() is boom, "the consumer output is retrieved"

    async def test_registration_panic_is_not_clean_completion(self) -> None:
        """A registration task that dies outside its own error surface (the
        Python analogue of a Rust task panic) is a registration failure, never
        a clean completion."""
        consumer = asyncio.create_task(_hanging_consumer())
        panic_message = "reg panic"

        async def panicking_registration() -> None:  # ruff: ignore[unused-async] -- a panicking task double
            raise _RegistrationPanic(panic_message)

        registration = asyncio.create_task(panicking_registration())
        watch = _watch(consumer, _never_watchdog, registration)
        await asyncio.sleep(0)

        assert await watch.wait() is SessionEndVerdict.RegistrationFailed
        assert str(watch.registration_error) == "reg panic"
        assert consumer.cancelled()

    async def test_watchdog_trip_ends_the_session_and_cancels_registration(
        self,
    ) -> None:
        """A real pump end outside ``stop()``: the watch's retrieval cancels
        and reaps the idling consumer, the gather cancels the still-pending
        registration, and the session leaves via the loud session-end abort
        (detection without an intended consumer end is the unexpected session
        end)."""
        consumer = asyncio.create_task(_hanging_consumer())

        async def climbing_registration() -> None:
            await asyncio.Event().wait()

        registration = asyncio.create_task(climbing_registration())
        watch = _watch(consumer, _pump_end_watchdog, registration)

        # No guard timeout here: the abort itself is the completion gate (the
        # detection fact ends the gather; the watch's retrieval then cancels and
        # reaps the consumer), matching the bare awaits its sibling verdict
        # tests use.
        with pytest.raises(RuntimeError, match="PumpFinished"):
            await watch.wait()
        assert consumer.cancelled()
        # The watch requested the registration cancel (the former twin's
        # watchdog-True branch)...
        assert registration.cancelling() >= 1
        # ...and the teardown duty drains it to a clean cancelled state.
        await watch.teardown()
        assert registration.cancelled()

    async def test_detection_without_an_intended_end_raises_the_session_end_abort(self) -> None:
        """Detection without an intended consumer end ranks the unexpected
        session end: the loud abort raises at the one session-end slot (the
        ``WatchdogTripped`` verdict always rides that slot)."""
        consumer = asyncio.create_task(_hanging_consumer())

        def instant_watchdog() -> Coroutine[Any, Any, str]:
            async def _watchdog() -> str:
                await asyncio.sleep(0)
                return "StallWatchdogTripped"

            return _watchdog()

        watch = _watch(consumer, instant_watchdog)

        with pytest.raises(RuntimeError, match="StallWatchdogTripped"):
            await watch.wait()
        assert consumer.cancelled()

    async def test_late_registration_failure_surfaces_with_hanging_watchdog(
        self,
    ) -> None:
        """A hanging watchdog (a real pump that never finishes) keeps the
        watch-set blocked while a LATER registration failure is still
        surfaced (never swallowed) via the fail-fast channel."""
        consumer = asyncio.create_task(_quick_consumer(ticks=5))
        boom = ValueError("late registration failure")

        async def late_failing_registration() -> None:
            for _ in range(2):
                await asyncio.sleep(0)
            raise boom

        registration = asyncio.create_task(late_failing_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        assert await watch.wait() is SessionEndVerdict.RegistrationFailed
        assert watch.registration_error is boom
        assert consumer.cancelled()

    async def test_hanging_watchdog_with_consumer_alone_still_ends(self) -> None:
        """{consumer} + never-finishing watchdog (injected engine): the
        session still ends through the consumer — a pending watchdog is not a
        hang and not a pump end."""
        consumer = asyncio.create_task(_quick_consumer(ticks=3))
        watch = _watch(consumer, _never_watchdog)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        assert consumer.done()
        assert not consumer.cancelled()


class TestIntendedEndTies:
    """The tie: an intended consumer end disarms the simultaneous detection
    fact (the expected close publishes ``PumpFinished`` too)."""

    async def test_an_intended_end_disarms_the_simultaneous_detection_fact(self) -> None:
        consumer = asyncio.create_task(_report_consumer("quiet-end")())

        def instant_watchdog() -> Coroutine[Any, Any, str]:
            async def _watchdog() -> str:
                await asyncio.sleep(0)
                return "PumpFinished"

            return _watchdog()

        watch = _watch(consumer, instant_watchdog)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        assert consumer.done()
        assert not consumer.cancelled()
        assert watch.consumer_output == {"batches": 0, "end": "quiet-end"}

    async def test_a_pump_stopped_report_is_the_unexpected_session_end(self) -> None:
        """A ``pump-stopped`` consumer outcome selects the watchdog verdict
        and raises its own loud abort at the one session-end slot."""
        consumer = asyncio.create_task(_report_consumer("pump-stopped")())
        watch = _watch(consumer, _never_watchdog)

        with pytest.raises(RuntimeError) as excinfo:
            await watch.wait()
        assert str(excinfo.value) == _PUMP_STOPPED_MESSAGE
        # The retained consumer output is not raised again over it.
        assert watch.consumer_output == {"batches": 0, "end": "pump-stopped"}

    async def test_the_consumer_output_is_retrieved_and_retained(self) -> None:
        consumer = asyncio.create_task(_report_consumer("quiet-end")())
        watch = _watch(consumer, _never_watchdog)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        assert watch.consumer_output == {"batches": 0, "end": "quiet-end"}


class TestConsumerEndTranslation:
    """The ``ConsumerEnd`` translation is exhaustive: exactly the two FFI
    spellings translate to core consumer outcomes, and anything else is
    refused loudly at the boundary.
    """

    @pytest.mark.parametrize("unknown_end", ["flubber-end", None])
    async def test_an_unknown_consumer_end_fails_loud_before_ranking(
        self,
        unknown_end: str | None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """A report whose ``end`` is not a ``ConsumerEnd`` spelling is refused
        at the translation - before the core's Session-end ranking can fold an
        unknown ending into the intended close (``QuietEnd`` / ``PumpEnded`` +
        ``Nothing``) - and the refusal leaves no unobserved task exception.

        The rank fold is tripped, not scripted: crossing it fails the test.
        """

        consumer = asyncio.create_task(_report_consumer(unknown_end)())
        watch = _watch(consumer, _never_watchdog)

        def _rank_must_not_run(**_facts: Any) -> Any:
            msg = "the rank fold must not run for an unknown end"
            raise AssertionError(msg)

        monkeypatch.setattr(_session_watch, "session_end_rank", _rank_must_not_run)

        with pytest.raises(ValueError, match="unknown consumer end") as excinfo:
            await watch.wait()

        assert repr(unknown_end) in str(excinfo.value)
        assert consumer.done()
        assert not consumer.cancelled()
        assert consumer.exception() is None
        assert consumer.result() == {"batches": 0, "end": unknown_end}


class TestWatchSetRecord:
    """The typed watch-set record the GLOSSARY.md session watch owns."""

    async def test_members_drop_registration_once_it_completes(self) -> None:
        async def _noop() -> None:  # ruff: ignore[unused-async] -- an instant task double
            return None

        consumer = asyncio.create_task(_hanging_consumer())
        watchdog = asyncio.create_task(_noop())
        registration = asyncio.create_task(_noop())
        watch = _WatchSet(consumer, watchdog, registration)
        assert watch.members() == {consumer, watchdog, registration}
        assert watch.without_registration().members() == {consumer, watchdog}

        for task in (consumer, watchdog, registration):
            task.cancel()


class TestCancellationObservation:
    """The reap discipline: a cancel the watch requested must be OBSERVED.

    Each reap ends in ``assert task.cancelled()`` (via ``cancel_and_reap``),
    so a swallowed cancel fails loudly instead of leaving a lost cancellation
    invisible.
    """

    async def test_fail_fast_observes_consumer_cancellation(self) -> None:
        """The fail-fast reap asserts the consumer's cancellation: a consumer
        that absorbs the cancel cannot exit ``wait()`` silently."""
        consumer = asyncio.create_task(_cancellation_absorbing_consumer())
        boom = ValueError("verification mismatch")

        async def failing_registration() -> None:  # ruff: ignore[unused-async] -- a raising task double
            raise boom

        registration = asyncio.create_task(failing_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        with pytest.raises(AssertionError, match="cancellation"):
            await watch.wait()

    async def test_watchdog_reap_observes_cancellation(self) -> None:
        """``wait()``'s exit reap asserts the watchdog's cancellation."""
        consumer = asyncio.create_task(_quick_consumer())
        watch = _watch(consumer, _absorbing_watchdog_factory)

        with pytest.raises(AssertionError, match="cancellation"):
            await watch.wait()

    async def test_teardown_registration_observes_cancellation(self) -> None:
        """The registration drain asserts the cancellation it requested."""
        consumer = asyncio.create_task(_hanging_consumer())
        registration = asyncio.create_task(_cancellation_absorbing_consumer())
        watch = _watch(consumer, _never_watchdog, registration)
        await asyncio.sleep(0)  # start the body so the cancel lands mid-await

        with pytest.raises(AssertionError, match="cancellation"):
            await watch.teardown_registration()


class TestSessionEndAbort:
    """The session-end abort's message names the ranked facts truthfully."""

    def test_the_abort_names_the_detection_cause_that_fired(self) -> None:
        for cause in ("PumpFinished", "StallWatchdogTripped"):
            abort = _session_end_abort(cause, "Failed")
            assert cause in str(abort)
            assert "without an intended consumer end" in str(abort)

    def test_the_pump_stopped_abort_keeps_its_operator_message(self) -> None:
        abort = _session_end_abort(None, "PumpStopped")
        assert str(abort) == _PUMP_STOPPED_MESSAGE

    def test_the_empty_remainder_message_says_nothing_fired(self) -> None:
        abort = _session_end_abort(None, None)
        assert "no consumer end and no detection fact" in str(abort)


class TestTeardown:
    async def test_teardown_cancels_pending_consumer_and_registration(self) -> None:
        """The ``__aexit__`` consumer-cancel + ``run()``-finally registration
        drain duties with ``wait()`` never called (a run() that raised before
        the main loop)."""
        consumer = asyncio.create_task(_hanging_consumer())

        async def climbing_registration() -> None:
            await asyncio.Event().wait()

        registration = asyncio.create_task(climbing_registration())
        watch = _watch(consumer, _never_watchdog, registration)

        await watch.teardown()
        assert consumer.cancelled()
        assert registration.cancelled()

    async def test_teardown_is_idempotent(self) -> None:
        """A second ``teardown()`` is a no-op — nothing to cancel, no raise."""
        consumer = asyncio.create_task(_quick_consumer())
        watch = _watch(consumer, _never_watchdog)

        assert await watch.wait() is SessionEndVerdict.PumpEnded
        await watch.teardown()
        await watch.teardown()
        assert consumer.done()

    async def test_teardown_with_no_attached_tasks_is_a_noop(self) -> None:
        """The ``__aexit__``-before-``run()`` shape: a bare watch tears down
        nothing and does not raise."""
        watch = SessionWatch()
        await watch.teardown()
