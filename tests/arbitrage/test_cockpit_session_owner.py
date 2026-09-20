"""FJA2Z7: session state as the cockpit's one owner (candidate 5).

The ``_SessionState`` built in ``start()`` is the ONE owner of the cockpit's
coordination values — the actors, the dispatcher, the block clock, the sim
context, and the two pipelines. The runner keeps no frozen mirrors
(``current_block`` / ``_sim_ctx`` used to freeze at ``start()`` while the
session advanced — a silent-staleness hazard), ``_Phase`` alone owns
lifecycle legality (no ``_started`` bool), and the consumer's remote
mutations ride the owner's mutators (``attach_pipeline`` /
``advance_block``), never attribute pokes.

Seams: the public BotRunner lifecycle (per-phase guard matrix, re-entry
idempotence) and the private loop seam (``consume_result_batches`` driven
with the runner-built owner + injected streams). No anvil, no live RPC.
"""

from __future__ import annotations

import contextlib
import signal

import pytest

from degenbot._ffi import session_phase_next
from degenbot.runner import BotRunner
from degenbot.runner._consume import consume_result_batches
from degenbot.runner.bot_runner import InjectedActors, PhaseError, _Phase, _SessionState
from degenbot.runner.config import ArbitrageConfig
from tests.fakes.engine import FakeEngineRegistry as _FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def _cfg() -> ArbitrageConfig:
    return ArbitrageConfig.from_env(
        {
            "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
            "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
            "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
            "INJECT_EXECUTOR_CODE": "0",
        },
        live=True,
        permutation=None,
    )


class _FakeBot:
    chain_id = 1

    def release_python_state(self) -> None:
        pass

    def block_stream(self):  # pragma: no cover - the noop consumer never iterates it
        # The injected consumer is the noop ``_noop()`` in every test here, so
        # run() never iterates this stream; ``None`` is the honest empty value.
        return None


class _FakeAsyncW3:
    async def get_block(self, block_identifier: str):
        return {"number": 12_345, "baseFeePerGas": 10**9, "gasUsed": 0, "gasLimit": 30_000_000}

    async def get_transaction_count(self, address: str) -> int:
        return 7

    def as_async_alloy(self) -> None:
        return None


class _FakeDispatcher:
    """The dispatcher-clock surface the loop touches."""

    def __init__(self) -> None:
        self.current_block = 100

    def record_block_time(self, block_number: int, block_timestamp: int) -> None:
        pass

    def block_time_count(self) -> int:
        return 0

    def block_times_oldest(self) -> tuple[int, int]:  # pragma: no cover - gate off
        return (0, 0)

    def advance_block(self, block_number: int) -> None:
        self.current_block = block_number

    def discard_path(self, path_id: int) -> None:
        pass

    def block_timestamp_for(self, block_number: int) -> int | None:
        return 1_700_000_000


def _noop():
    async def _n() -> None:
        pass

    return _n()


def _runner(**overrides: object) -> BotRunner:
    actors: dict[str, object] = {
        "bot": _FakeBot(),
        "engine_registry": _FakeEngineRegistry(),
        "async_w3": _FakeAsyncW3(),
        "snapshots": (object(), object(), None, None),
        "path_builder": lambda **kw: _noop(),
        "consumer": lambda **kw: _noop(),
    }
    install_sigint = overrides.pop("install_sigint", False)
    actors.update(overrides)
    return BotRunner(  # type: ignore[arg-type]
        _cfg(), actors=InjectedActors(**actors), install_sigint=install_sigint
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


@pytest.fixture(name="stub_pipeline")
def _stub_pipeline(monkeypatch: pytest.MonkeyPatch) -> type[StubPipeline]:
    monkeypatch.setattr("degenbot.runner._consume.SimSubmitPipeline", StubPipeline)
    return StubPipeline


async def _drive_one_block(session: _SessionState, *, tick: int) -> None:
    """Run the real consumer loop over one block tick + one empty batch."""
    await consume_result_batches(
        session,
        block_stream=AsyncOnce(_block_tick(tick)),
        result_iter=AsyncOnce(_empty_batch()),
        allow_quiet_end=True,  # injected one-shot streams end by design
    )


class TestSessionBuiltRealInStart:
    """The session is REAL at construction — no residual Option cluster."""

    async def test_start_builds_the_session_with_real_values(self) -> None:
        registry = _FakeEngineRegistry()
        runner = _runner(engine_registry=registry)
        assert runner._session is None  # no session before start()

        await runner.start()

        session = runner._session
        assert session is not None
        assert session.engine_registry is registry
        assert session.bot is not None
        assert session.async_w3 is not None
        assert session.dispatcher is not None
        assert session.cfg is runner.cfg
        assert session.current_block == 12_345
        # The runner facade reads the SAME objects (no copies).
        assert runner.bot is session.bot
        assert runner.engine_registry is registry
        assert runner.dispatcher is session.dispatcher


class TestStartReentryIdempotence:
    """Re-entry idempotence is owned by _Phase ALONE (no _started bool)."""

    async def test_reentry_in_started_is_a_noop(self) -> None:
        registry = _FakeEngineRegistry()
        runner = _runner(engine_registry=registry)
        first = await runner.start()
        session_after_first = runner._session

        second = await runner.start()

        assert second is first is runner
        assert len(registry.start_calls) == 1, "re-entry must not rebuild the actors"
        assert runner._session is session_after_first

    async def test_reentry_in_running_raises_phase_error(self) -> None:
        runner = _runner()
        await runner.start()
        await runner.run()
        assert runner._phase is _Phase.RUNNING

        with pytest.raises(PhaseError, match="start\\(\\) in phase 'running'"):
            await runner.start()

    async def test_reentry_in_closed_raises_phase_error(self) -> None:
        runner = _runner()
        async with runner:
            await runner.run()
        assert runner._phase is _Phase.CLOSED

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

    async def test_consumer_advanced_block_visible_through_every_reader(
        self, stub_pipeline: type[StubPipeline]
    ) -> None:
        runner = _runner()
        await runner.start()
        session = runner._session
        assert session is not None
        start_block = session.current_block

        await _drive_one_block(session, tick=start_block + 2)

        # The owner advanced (the loop's advance_block mutator).
        assert session.current_block == start_block + 2
        # The dispatcher clock agrees (driven by the same tick).
        assert session.dispatcher.current_block == start_block + 2
        # NO frozen runner-side mirror may shadow the owner: if a mirror
        # attribute reappears on the runner, it must track the owner, not
        # freeze at the start() value (pre-change: runner.current_block froze
        # at start_block while the session advanced).
        assert getattr(runner, "current_block", session.current_block) == start_block + 2
        assert getattr(runner, "_sim_ctx", session.sim_ctx) is session.sim_ctx

    async def test_pipeline_attach_visible_through_the_owner(
        self, stub_pipeline: type[StubPipeline]
    ) -> None:
        runner = _runner()
        await runner.start()
        session = runner._session
        assert session is not None

        await _drive_one_block(session, tick=session.current_block + 1)

        # The consumer's attach rode the owner's mutator — the pipeline is
        # visible on the owner (not stashed on the runner).
        assert session.sim_submit_pipeline is not None
        assert session.sim_submit_pipeline.session is session

    async def test_advance_block_is_the_one_block_mutation(self) -> None:
        session = _SessionState(
            engine_registry=_FakeEngineRegistry(),  # type: ignore[arg-type]
            async_w3=_FakeAsyncW3(),  # type: ignore[arg-type]
            sim_ctx=None,
            dispatcher=_FakeDispatcher(),
            cfg=_cfg(),
            current_block=100,
        )
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
    async def _drive_to(phase: _Phase) -> BotRunner:
        runner = _runner()
        if phase is _Phase.NEW:
            return runner
        await runner.start()
        if phase is _Phase.STARTED:
            return runner
        await runner.run()
        if phase is _Phase.RUNNING:
            return runner
        await runner.shutdown()
        return runner

    @pytest.mark.parametrize(
        "method",
        ["start", "run", "enqueue_path", "trigger_discovery", "shutdown"],
    )
    @pytest.mark.parametrize("phase", list(_Phase))
    async def test_runner_honors_the_rust_session_table(self, phase: _Phase, method: str) -> None:
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
            assert runner._phase is phase, "a refused move leaves the phase untouched"
        else:
            # A legal query still raises when no live pipeline exists
            # (injected/fake runs) — that is past the phase gate.
            with contextlib.suppress(RuntimeError):
                await call()
            assert runner._phase.value == expected


class TestStopFunnel:
    """Stop has ONE engine-level entrypoint for both Python entrances."""

    async def test_shutdown_and_sigint_share_the_stop_entrypoint(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner = _runner(install_sigint=True)
        await runner.start()
        calls: list[str] = []
        monkeypatch.setattr(runner, "_stop_engine", lambda: calls.append("stop"))

        await runner.shutdown()
        with pytest.raises(KeyboardInterrupt):
            runner._handle_sigint(signal.SIGINT, None)

        assert calls == ["stop", "stop"], (
            "shutdown() and the SIGINT handler must both funnel through BotRunner._stop_engine"
        )
