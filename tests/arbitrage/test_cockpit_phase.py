"""Typed cockpit phase machine (epic Y7PA5A, task 5OV35X).

The driver cockpit enforces its startup ordering through a private
``_Phase`` machine: New -> Started -> Running -> Closed. Illegal
sequencing raises :class:`PhaseError` instead of failing obscurely, and
``shutdown()`` stays deliberately any-phase/idempotent (the SIGINT
teardown ordering depends on it).

Drove through the public BotRunner lifecycle only — injected fakes for
bot / engine_registry / async_w3 / path_builder / consumer (the same
seam pattern as ``test_arbitrage_session.py``); no anvil, no live RPC.
"""

from __future__ import annotations

import signal

import pytest

from degenbot.runner import BotRunner
from degenbot.runner.bot_runner import PhaseError
from degenbot.runner.config import ArbitrageConfig


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Chain-1 RPC envvars must be present for ``from_env`` (never connected)."""
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


class _FakeEngine:
    def resume(self) -> None:
        pass

    def stop(self) -> None:
        pass

    def v2_pool_count(self) -> int:
        return 0

    def v3_pool_count(self) -> int:
        return 0

    def v4_pool_count(self) -> int:
        return 0

    def path_count(self) -> int:
        return 0

    async def block_stream(self):
        return
        yield  # pragma: no cover - async generator marker


class _FakeEngineRegistry:
    def __init__(self) -> None:
        self.engine = _FakeEngine()
        self.start_calls = 0

    def start(self, node_http, node_ws, *, v3_snapshot, v4_snapshot, verify_state_view) -> int:
        self.start_calls += 1
        return 12_000


class _FakeBot:
    chain_id = 1

    def release_python_state(self) -> None:
        pass


class _FakeAsyncW3:
    async def get_block(self, block_identifier: str):
        return {"number": 12_345, "baseFeePerGas": 10**9, "gasUsed": 0, "gasLimit": 30_000_000}

    def as_async_alloy(self) -> None:
        return None


def _session() -> BotRunner:
    return BotRunner(
        _cfg(),
        bot=_FakeBot(),
        engine_registry=_FakeEngineRegistry(),
        async_w3=_FakeAsyncW3(),
        snapshots=(object(), object(), None, None),
        path_builder=lambda **kw: _noop(),
        consumer=lambda **kw: _noop(),
        install_sigint=False,
    )


def _noop():
    async def _n() -> None:
        pass

    return _n()


class TestPhaseMachine:
    async def test_enqueue_path_before_run_raises_phase_error(self) -> None:
        async with _session() as session:
            with pytest.raises(PhaseError):
                await session.enqueue_path([], directions=None)

    async def test_trigger_discovery_before_run_raises_phase_error(self) -> None:
        async with _session() as session:
            with pytest.raises(PhaseError):
                await session.trigger_discovery()

    async def test_double_run_raises_phase_error(self) -> None:
        async with _session() as session:
            await session.run()
            with pytest.raises(PhaseError):
                await session.run()

    async def test_double_start_is_idempotent_no_op(self) -> None:
        session = _session()
        registry = session._injected_engine_registry
        first = await session.start()
        second = await session.start()
        assert first is session
        assert second is session
        assert registry.start_calls == 1

    async def test_shutdown_before_start_does_not_raise(self) -> None:
        session = _session()
        await session.shutdown()

    async def test_closed_session_cannot_run(self) -> None:
        session = _session()
        async with session:
            await session.run()
        # __aexit__ -> shutdown() -> Closed: run() is now a typed error.
        with pytest.raises(PhaseError):
            await session.run()

    async def test_closed_session_cannot_enqueue(self) -> None:
        session = _session()
        async with session:
            pass
        with pytest.raises(PhaseError):
            await session.enqueue_path([], directions=None)
