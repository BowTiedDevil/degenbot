"""Session state as the cockpit's one owner (candidate 5).

The session a boot builds is the ONE owner of the cockpit's coordination values
— the actors, the dispatcher, the block clock, the sim context, and the two
pipelines. The runner keeps no mirrors (``current_block`` / ``_sim_ctx`` used
to freeze at ``start()`` while the session advanced — a silent-staleness
hazard), ``SessionPhase`` alone owns lifecycle legality (no ``_started`` bool),
and the consumer's mutations ride the owner's mutators (``build_pipeline`` /
``advance_block``), never attribute pokes.

Seams: the public ``BotRunner`` lifecycle (production start over the DI record;
per-phase guard matrix, re-entry idempotence) and the loop seam
(``consume_result_batches`` driven with the session's own collaborator record +
injected streams). No anvil, no live RPC.
"""

from __future__ import annotations

import contextlib
import signal

import pytest

from degenbot.runner import BotRunner
from degenbot.runner._consume import consume_result_batches
from degenbot.runner.session import PhaseError, SessionPhase
from tests.fakes.engine import FakeEngineRegistry
from tests.helpers.seam import session_phase_next
from tests.helpers.session import (
    FakeDispatcher,
    injected_test_actors,
    offline_cfg,
    session_for_test,
)


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def _runner(**overrides: object) -> BotRunner:
    install_sigint = overrides.pop("install_sigint", False)
    return BotRunner(
        offline_cfg(),
        actors=injected_test_actors(settlement_arm=True, **overrides),  # type: ignore[arg-type]
        install_sigint=install_sigint,  # type: ignore[arg-type]
    )


class AsyncOnce:
    """Yield one item, then StopAsyncIteration."""

    def __init__(self, item: object) -> None:
        self._item = item
        self._done = False

    def __aiter__(self) -> AsyncOnce:
        return self

    async def __anext__(self) -> object:
        if self._done:
            raise StopAsyncIteration
        self._done = True
        return self._item


def _block_tick(number: int) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _empty_batch() -> dict[str, object]:
    return {
        "fresh": [],
        "updated": [],
        "removed": [],
        "solve_block": 12_346,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


async def _drive_one_block(session: object, *, tick: int) -> None:
    """Run the real consumer loop over one block tick + one empty batch."""
    await consume_result_batches(
        session.block_loop(),  # type: ignore[attr-defined]
        block_stream=AsyncOnce(_block_tick(tick)),
        result_iter=AsyncOnce(_empty_batch()),
        allow_quiet_end=True,  # injected one-shot streams end by design
    )


class TestSessionBuiltRealInStart:
    """The boot fills the session in exactly once — no residual option cluster."""

    async def test_start_builds_the_session_with_real_values(self) -> None:
        registry = FakeEngineRegistry()
        runner = _runner(engine_registry=registry)
        # Before start() the session exists (it owns the lifecycle) but holds
        # no built state: the phase is New and no actors were resolved.
        assert runner.session.phase is SessionPhase.NEW
        assert runner.session.actors is None

        await runner.start()

        session = runner.session
        assert session.phase is SessionPhase.STARTED
        assert session.actors is not None
        assert session.actors.engine_registry is registry
        assert session.actors.bot is not None
        assert session.actors.async_w3 is not None
        assert session.dispatcher is not None
        assert session.cfg is runner.session.cfg
        assert session.current_block == 12_345


class TestStartReentryIdempotence:
    """Re-entry idempotence is owned by SessionPhase ALONE (no _started bool)."""

    async def test_reentry_in_started_is_a_noop(self) -> None:
        registry = FakeEngineRegistry()
        runner = _runner(engine_registry=registry)
        first = await runner.start()
        actors_after_first = runner.session.actors

        second = await runner.start()

        assert second is first is runner
        assert len(registry.start_calls) == 1, "re-entry must not rebuild the actors"
        assert runner.session.actors is actors_after_first

    async def test_reentry_in_running_raises_phase_error(self) -> None:
        runner = _runner()
        await runner.start()
        await runner.run()
        assert runner.session.phase is SessionPhase.RUNNING

        with pytest.raises(PhaseError, match="start\\(\\) in phase 'running'"):
            await runner.start()

    async def test_reentry_in_closed_raises_phase_error(self) -> None:
        runner = _runner()
        async with runner:
            await runner.run()
        assert runner.session.phase is SessionPhase.CLOSED

        with pytest.raises(PhaseError, match="start\\(\\) in phase 'closed'"):
            await runner.start()

    async def test_start_after_shutdown_before_start_raises(self) -> None:
        # The preserved reachable path: shutdown() is any-phase, so a NEW
        # session closed without start() refuses a later start().
        runner = _runner()
        await runner.shutdown()
        with pytest.raises(PhaseError, match="start\\(\\) in phase 'closed'"):
            await runner.start()


class TestNoFrozenMirrors:
    """A consumer-advanced block is visible through EVERY reader.

    Pre-change the runner kept frozen-at-start() copies (``current_block`` /
    ``_sim_ctx``) that silently diverged from the owner the consumer advances.
    """

    async def test_consumer_advanced_block_visible_through_the_session(self) -> None:
        session = session_for_test(dispatcher=FakeDispatcher(current_block=12_345))
        await session.build_pipeline()
        start_block = session.current_block

        await _drive_one_block(session, tick=start_block + 2)

        # The owner advanced (the loop's advance_block mutator).
        assert session.current_block == start_block + 2
        # The dispatcher clock agrees (driven by the same tick).
        assert session.dispatcher is not None
        assert session.dispatcher.current_block == start_block + 2

    async def test_pipeline_attach_rides_the_owner(self) -> None:
        session = session_for_test()
        await session.build_pipeline()

        await _drive_one_block(session, tick=session.current_block + 1)

        # The session's pipeline is visible on the owner (not stashed on a
        # facade), and it was built over that same owner.
        assert session.sim_submit_pipeline is not None
        assert session.sim_submit_pipeline.session is session  # type: ignore[attr-defined]

    async def test_advance_block_is_the_one_block_mutation(self) -> None:
        session = session_for_test(current_block=100)
        session.advance_block(105)
        assert session.current_block == 105


# The Python methods map onto the host's four session-phase operations.
_RUST_OPERATION = {
    "start": "start",
    "run": "run",
    "enqueue_path": "query",
    "trigger_discovery": "query",
    "shutdown": "shutdown",
}


class TestPhaseGuardMatrix:
    """The guard matrix is the Rust host's session-phase table, not a Python one.

    Every cell is read from ``degenbot._ffi.session_phase_next`` (the host's
    ``SessionPhase`` table in ``strategy_host.rs``); the runner is then driven
    to that phase and its lifecycle method must honor the host verdict. A
    matrix change that lands in only one language cannot stay green.
    """

    @staticmethod
    async def _drive_to(phase: SessionPhase) -> BotRunner:
        runner = _runner()
        if phase is SessionPhase.NEW:
            return runner
        await runner.start()
        if phase is SessionPhase.STARTED:
            return runner
        await runner.run()
        if phase is SessionPhase.RUNNING:
            return runner
        await runner.shutdown()
        return runner

    @pytest.mark.parametrize(
        "method",
        ["start", "run", "enqueue_path", "trigger_discovery", "shutdown"],
    )
    @pytest.mark.parametrize("phase", list(SessionPhase))
    async def test_runner_honors_the_rust_session_table(
        self, phase: SessionPhase, method: str
    ) -> None:
        expected = session_phase_next(phase.value, _RUST_OPERATION[method])
        runner = await self._drive_to(phase)
        call = {
            "start": runner.start,
            "run": runner.run,
            "enqueue_path": lambda: runner.enqueue_path([], directions=None),
            "trigger_discovery": runner.trigger_discovery,
            "shutdown": runner.shutdown,
        }[method]
        if expected is None:
            with pytest.raises(PhaseError):
                await call()
            assert runner.session.phase is phase, "a refused move leaves the phase untouched"
        else:
            # A legal query still raises when no live pipeline exists
            # (injected/fake runs) — that is past the phase gate.
            with contextlib.suppress(RuntimeError):
                await call()
            assert runner.session.phase.value == expected


class TestStopFunnel:
    """Stop has ONE engine-level entrypoint for both Python entrances."""

    async def test_shutdown_and_sigint_share_the_stop_entrypoint(self) -> None:
        calls: list[str] = []
        runner = _runner(
            install_sigint=True,
            stop_engine=lambda: calls.append("stop"),
        )
        await runner.start()

        await runner.shutdown()
        with pytest.raises(KeyboardInterrupt):
            runner.session._handle_sigint(signal.SIGINT, None)

        assert calls == ["stop", "stop"], (
            "shutdown() and the SIGINT handler must both funnel through Session._stop_engine"
        )
