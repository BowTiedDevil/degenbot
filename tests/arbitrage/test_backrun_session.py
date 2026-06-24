"""Tests for BackrunSession — the orchestrator that collapses main()'s startup ritual.

`BackrunSession` owns the config + the three actors (bot, engine_registry,
async_w3) + the Dispatcher + scalar block/nonce state, and is the ONE place
that enforces the phase ordering: subscribe→backfill→verify
(``EngineRegistry.start``, stops pre-resume) → [``run``:] attach consumer →
``resume()`` → ``build_paths`` → ``release_python_state`` → main loop.

The full handshake is I/O-bound (live RPC/WS/DB), so these tests inject fakes
for bot/engine_registry/async_w3 + the two module functions (path_builder,
consumer) — the OLKZ3L ``engine=`` seam pattern scaled up. The production
path (all None) builds from cfg and calls the real functions; verified by the
example running (the example IS the integration test).
"""

from __future__ import annotations

import asyncio

import pytest

from examples.eth_backrun_helpers import BackrunConfig
from examples.eth_backrun_v2_v3_v4_rust import BackrunSession


def _cfg(**overrides) -> BackrunConfig:
    base = {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "NODE_HOST_HTTP": "http://localhost",
        "NODE_PORT_HTTP": "8545",
        "NODE_HOST_WEBSOCKET": "ws://localhost",
        "NODE_PORT_WEBSOCKET": "8546",
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
    }
    base.update(overrides)
    return BackrunConfig.from_env(base, live=True, permutation=None)


class _FakeEngine:
    def __init__(self, events: list[str] | None = None) -> None:
        self.resumed = False
        self._events = events

    def resume(self) -> None:
        self.resumed = True
        if self._events is not None:
            self._events.append("resume")

    def v2_pool_count(self) -> int:
        return 0

    def v3_pool_count(self) -> int:
        return 0

    def v4_pool_count(self) -> int:
        return 0

    def path_count(self) -> int:
        return 0


class _FakeEngineRegistry:
    def __init__(self, *, backfill_target: int = 12_000, events: list[str] | None = None) -> None:
        self.engine = _FakeEngine(events=events)
        self.start_calls: list[dict] = []
        self._backfill_target = backfill_target

    def start(
        self,
        node_http,
        node_ws,
        *,
        v3_snapshot,
        v4_snapshot,
        verify_state_view,
        verify_on_register,
    ) -> int:
        self.start_calls.append({
            "node_http": node_http,
            "node_ws": node_ws,
            "v3_snapshot": v3_snapshot,
            "v4_snapshot": v4_snapshot,
            "verify_state_view": verify_state_view,
            "verify_on_register": verify_on_register,
        })
        return self._backfill_target

    def verify_liquidity_maps(self, *, block_number=None) -> None:
        self.verify_call = block_number


class _FakeBot:
    def __init__(self, events: list[str] | None = None) -> None:
        self.chain_id = 1
        self.released = False
        self._events = events

    def release_python_state(self) -> None:
        self.released = True
        if self._events is not None:
            self._events.append("release")


class _FakeEth:
    def __init__(self, *, block_number: int = 12_345, nonce: int = 7) -> None:
        self._block_number = block_number
        self._nonce = nonce

    async def get_block(self, block_identifier: str):
        return {
            "number": self._block_number,
            "baseFeePerGas": 10**9,
            "gasUsed": 0,
            "gasLimit": 30_000_000,
        }

    async def get_transaction_count(self, address: str):
        return self._nonce


class _FakeAsyncW3:
    def __init__(self) -> None:
        self.eth = _FakeEth()


def _noop_coro():
    async def _n() -> None:
        pass

    return _n()


class _Recorder:
    """Records call order via an injected events list; returns a no-op coroutine.

    The append fires synchronously at __call__ time (i.e. at ``create_task``
    argument evaluation for the consumer, or at the ``await`` for the path
    builder), so the recorded order reflects run()'s call sequence, not the
    consumer task's lazy execution.
    """

    def __init__(self, events: list[str], tag: str) -> None:
        self._events = events
        self._tag = tag

    def __call__(self, **kwargs):
        self._events.append(self._tag)
        return _noop_coro()


class TestBackrunSessionStart:
    async def test_start_orchestrates_pre_resume_ritual(self) -> None:
        events: list[str] = []
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry(backfill_target=12_000)
        async_w3 = _FakeAsyncW3()
        v3_snap, v4_snap = object(), object()
        snapshots = (v3_snap, v4_snap, None, None)

        session = BackrunSession(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            snapshots=snapshots,
            path_builder=_Recorder(events, "path_builder"),
            consumer=_Recorder(events, "consumer"),
        )

        await session.start()

        # engine_registry.start called with cfg's node URLs + the injected snapshots
        assert len(engine_registry.start_calls) == 1
        call = engine_registry.start_calls[0]
        assert call["node_http"] == "http://localhost:8545"
        assert call["node_ws"] == "ws://localhost:8546"
        assert call["v3_snapshot"] is v3_snap
        assert call["v4_snapshot"] is v4_snap
        assert call["verify_on_register"] is True
        # verify_state_view is the V4 state view constant (non-empty)
        assert call["verify_state_view"].startswith("0x")

        # dispatcher seeded at the fetched current_block
        assert session.dispatcher.current_block == 12_345

        # CRITICAL invariant: start() does NOT resume the pump
        assert engine_registry.engine.resumed is False

    async def test_start_advances_current_block_when_backfill_ahead(self) -> None:
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry(backfill_target=13_000)  # ahead of latest 12_345
        async_w3 = _FakeAsyncW3()

        session = BackrunSession(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            snapshots=(None, None, None, None),
            path_builder=lambda **kw: _noop_coro(),
            consumer=lambda **kw: _noop_coro(),
        )
        await session.start()

        assert session.dispatcher.current_block == 13_000


class TestBackrunSessionRun:
    async def test_run_enforces_phase_ordering(self) -> None:
        events: list[str] = []
        bot = _FakeBot(events=events)
        engine_registry = _FakeEngineRegistry(backfill_target=12_000, events=events)
        async_w3 = _FakeAsyncW3()
        v3_snap, v4_snap = object(), object()

        session = BackrunSession(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            snapshots=(v3_snap, v4_snap, None, None),
            path_builder=_Recorder(events, "path_builder"),
            consumer=_Recorder(events, "consumer"),
        )
        await session.start()
        events.clear()  # only observe run()'s sequence

        await session.run()

        # consumer task created FIRST (before resume), then resume, then paths, then release
        assert events == ["consumer", "resume", "path_builder", "release"]
        assert engine_registry.engine.resumed is True
        assert bot.released is True

    async def test_run_drops_bot_before_main_loop(self) -> None:
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry()
        async_w3 = _FakeAsyncW3()

        session = BackrunSession(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            snapshots=(None, None, None, None),
            path_builder=lambda **kw: _noop_coro(),
            consumer=lambda **kw: _noop_coro(),
        )
        await session.start()
        await session.run()

        # bot released + dropped; engine_registry survives (holds its own PyBot ref)
        assert bot.released is True
        assert session.bot is None
        assert session.engine_registry is engine_registry


class TestBackrunSessionContextManager:
    async def test_aexit_cancels_consumer_when_run_raises(self) -> None:
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry()
        async_w3 = _FakeAsyncW3()

        # a hanging consumer (pending forever) + a path_builder that raises
        async def hanging_consumer(**kwargs):
            await asyncio.Event().wait()

        async def raising_path_builder(**kwargs):  # noqa: RUF029
            boom = "build_paths failed"
            raise RuntimeError(boom)

        session = BackrunSession(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=async_w3,
            snapshots=(None, None, None, None),
            path_builder=raising_path_builder,
            consumer=hanging_consumer,
        )
        await session.start()

        boom = "build_paths failed"
        with pytest.raises(RuntimeError, match=boom):
            async with session:
                await session.run()

        # the consumer task must have been cancelled by __aexit__
        assert session._result_consumer_task is not None
        assert session._result_consumer_task.cancelled()
