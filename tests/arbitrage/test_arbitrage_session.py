"""Tests for BotRunner — the orchestrator that collapses main()'s startup ritual.

`BotRunner` owns the config + the three actors (bot, engine_registry,
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
import contextlib
from dataclasses import dataclass

import pytest

from degenbot.runner import BotRunner
from degenbot.runner.config import ArbitrageConfig


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Set chain-1 RPC OS envvars for every test in this module.

    The session tests inject fakes for bot/engine_registry/async_w3, so the RPC
    URIs are never connected — they only need to be *present* on the config so
    ``ArbitrageConfig.from_env`` does not raise ``RpcNotConfiguredError``. Keeping
    them ``http://localhost:8545`` / ``ws://localhost:8546`` preserves the legacy
    assertions in ``test_start_orchestrates_pre_resume_ritual``.
    """
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    """Restore the default SIGINT handler after each test.

    ``BotRunner.start()`` binds a SIGINT→``engine.stop()`` handler on the
    production path (``install_sigint=True``). Tests that call ``start()`` then
    ``run()`` directly (without ``async with``) never reach ``__aexit__``, so
    the handler would leak across tests and pollute ``signal.getsignal``
    assertions. This fixture restores ``SIG_DFL`` after each test unconditionally.
    """
    yield
    import signal as _signal

    _signal.signal(_signal.SIGINT, _signal.SIG_DFL)


def _cfg(**overrides) -> ArbitrageConfig:
    base = {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
    }
    base.update(overrides)
    return ArbitrageConfig.from_env(base, live=True, permutation=None)


class _FakeEngine:
    def __init__(self, events: list[str] | None = None) -> None:
        self.resumed = False
        self._events = events
        self.stop_calls = 0
        self.stop_raises: Exception | None = None

    def resume(self) -> None:
        self.resumed = True
        if self._events is not None:
            self._events.append("resume")

    def stop(self) -> None:
        self.stop_calls += 1
        if self._events is not None:
            self._events.append("stop")
        if self.stop_raises is not None:
            raise self.stop_raises

    def last_processed_block(self) -> int | None:
        return 12_345

    def v2_pool_count(self) -> int:
        return 0

    def v3_pool_count(self) -> int:
        return 0

    def v4_pool_count(self) -> int:
        return 0

    def path_count(self) -> int:
        return 0

    async def block_stream(self):
        # T7: the hot loop spawns a recurring-verify task consuming
        # engine.block_stream(). Test engines never advance blocks, so yield
        # nothing — the recurring verify task stays idle until cancelled.
        return
        yield  # pragma: no cover - makes this an async generator


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
    ) -> int:
        self.start_calls.append({
            "node_http": node_http,
            "node_ws": node_ws,
            "v3_snapshot": v3_snapshot,
            "v4_snapshot": v4_snapshot,
            "verify_state_view": verify_state_view,
        })
        return self._backfill_target


class _FakeBot:
    def __init__(self, events: list[str] | None = None) -> None:
        self.chain_id = 1
        self.released = False
        self._events = events

    def release_python_state(self) -> None:
        self.released = True
        if self._events is not None:
            self._events.append("release")


class _RecordingPyBot:
    """A stand-in for the Rust ``Bot._py_bot`` whose ``close_snapshot_tx``
    records invocation (and optionally trips the XEANMB canary RuntimeError,
    as the real one does when in-flight build workers hold an ``Arc`` clone)."""

    def __init__(
        self,
        events: list[str],
        *,
        raise_on_close: bool = False,
    ) -> None:
        self._events = events
        self._raise_on_close = raise_on_close

    def close_snapshot_tx(self) -> None:
        self._events.append("close_snapshot_tx")
        if self._raise_on_close:
            msg = (
                "close_snapshot_tx: SnapshotDb Arc still held "
                "(clone leak \u2014 a caller didn't drop its handle)"
            )
            raise RuntimeError(msg)


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
    """Fake ``AsyncAlloyProvider`` for BotRunner tests (PAGQCK).

    The dispatch hot loop was routed off raw ``AsyncWeb3`` onto
    ``AsyncAlloyProvider`` — this fake exposes the SAME flat surface
    (``get_block`` / ``get_transaction_count`` / ``make_request`` / ``rpc_url``)
    the example now drives, delegating to the inner ``_FakeEth``.
    """

    def __init__(self) -> None:
        self.eth = _FakeEth()

    async def get_block(self, block_identifier: str):
        return await self.eth.get_block(block_identifier)

    async def get_transaction_count(self, address: str):
        return await self.eth.get_transaction_count(address)

    async def make_request(self, method: str, params: list):
        # The session tests don't drive the four typed dispatch RPCs
        # (simulate/create_access_list/send_raw_transaction); return an empty
        # result so any incidental make_request is a benign no-op.
        return {}

    @property
    def rpc_url(self) -> str:
        return "http://fake:8545"

    def as_async_alloy(self) -> None:
        # The session lifecycle tests don't drive dispatch (they inject a
        # `_Recorder` consumer that ignores args), so the alloy provider is
        # never used. `start()` tolerates `None` here (defers `_sim_ctx`);
        # only a session that actually dispatches needs a real provider.
        return None


def _noop_coro():
    async def _n() -> None:
        pass

    return _n()


class _BlocksStream:
    """Async iterator over a fixed list of block dicts, then StopAsyncIteration.

    Stand-in for the once-only `engine.block_stream()` receiver (mimics
    `BlockStream.__anext__`).
    """

    def __init__(self, blocks: list[dict[str, int]]) -> None:
        self._blocks = list(blocks)
        self._i = 0

    def __aiter__(self) -> _BlocksStream:
        return self

    async def __anext__(self) -> dict[str, int]:
        if self._i >= len(self._blocks):
            raise StopAsyncIteration
        b = self._blocks[self._i]
        self._i += 1
        return b


def _block_dict(number: int) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


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


class TestBotRunnerStart:
    async def test_start_orchestrates_pre_resume_ritual(self) -> None:
        events: list[str] = []
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry(backfill_target=12_000)
        async_w3 = _FakeAsyncW3()
        v3_snap, v4_snap = object(), object()
        snapshots = (v3_snap, v4_snap, None, None)

        session = BotRunner(
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

        session = BotRunner(
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


class TestBotRunnerRun:
    async def test_run_enforces_phase_ordering(self) -> None:
        events: list[str] = []
        bot = _FakeBot(events=events)
        engine_registry = _FakeEngineRegistry(backfill_target=12_000, events=events)
        async_w3 = _FakeAsyncW3()
        v3_snap, v4_snap = object(), object()

        session = BotRunner(
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

        session = BotRunner(
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

        # bot released + dropped; engine_registry survives (holds its own Bot ref)
        assert bot.released is True
        assert session.bot is None
        assert session.engine_registry is engine_registry

    async def test_aexit_cancels_consumer_when_run_raises(self) -> None:
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry()
        async_w3 = _FakeAsyncW3()

        # a hanging consumer (pending forever) + a path_builder that raises
        async def hanging_consumer(**kwargs):
            await asyncio.Event().wait()

        async def raising_path_builder(**kwargs):  # ruff:ignore[unused-async]
            boom = "build_paths failed"
            raise RuntimeError(boom)

        session = BotRunner(
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


class TestBotRunnerRunBlockStreamAcquiredOnce:
    """Regression: `run()` must acquire the once-only `engine.block_stream()`
    exactly ONCE and feed it DIRECTLY to the single result consumer (the tee and
    the redundant Python recurring-verify branch were removed).

    The real `PyArbitrageEngine.block_stream()` is once-only — a second call
    raises `RuntimeError("block_stream() can only be called once")`. This test
    uses an engine whose `block_stream()` raises on the second call (mimicking
    the real once-only seam) and asserts the single consumer receives every
    block.
    """

    async def test_run_acquires_block_stream_once_for_single_consumer(
        self,
    ) -> None:
        seen_by_consumer: list[int] = []

        class _OnceOnlyEngine:
            """Mimics the real `block_stream()` once-only receiver semantics."""

            def __init__(self) -> None:
                self.block_stream_calls = 0
                self.resumed = False

            def resume(self) -> None:
                self.resumed = True

            def last_processed_block(self) -> int | None:
                return 12_345

            def v2_pool_count(self) -> int:
                return 0

            def v3_pool_count(self) -> int:
                return 0

            def v4_pool_count(self) -> int:
                return 0

            def path_count(self) -> int:
                return 0

            def block_stream(self):
                self.block_stream_calls += 1
                if self.block_stream_calls > 1:
                    msg = "block_stream() can only be called once"
                    raise RuntimeError(msg)
                # Blocks divisible by RECURRING_VERIFY_INTERVAL (50) so the
                # recurring-verify ticker actually fires at each.
                return _BlocksStream(
                    [_block_dict(500), _block_dict(550), _block_dict(600)],
                )

        class _Registry:
            def __init__(self) -> None:
                self.engine = _OnceOnlyEngine()

            def start(
                self, node_http, node_ws, *, v3_snapshot, v4_snapshot, verify_state_view
            ) -> int:
                # No backfill target beyond current_block (12_345) — main-loop entry.
                return 0

        registry = _Registry()

        async def recording_consumer(*, block_stream=None, **_kw) -> None:
            async for b in block_stream:
                seen_by_consumer.append(b["number"])  # ruff: ignore[manual-list-comprehension]  (async iter)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=registry,  # type: ignore[arg-type]
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=recording_consumer,
        )
        await session.start()
        await session.run()

        # The load-bearing contract: exactly ONE acquisition.
        assert registry.engine.block_stream_calls == 1, (
            "run() must acquire engine.block_stream() exactly once (was 2 → crash)"
        )
        assert registry.engine.resumed is True
        # The single consumer received every block.
        assert seen_by_consumer == [500, 550, 600], "result-consumer must receive every block"


class TestBotRunnerShutdown:
    """``BotRunner.shutdown()`` is the clean-shutdown seam that hands a
    Ctrl-C to the Rust core before the process exits.

    The pump task runs on the shared tokio runtime (decoupled from the asyncio
    loop), so a ``KeyboardInterrupt`` tearing down ``asyncio.run`` does not
    reach it — the pump keeps blocking on the WS stream, holding process exit
    for up to 60s on a silent subscription. ``shutdown()`` closes that gap by
    calling ``engine.stop()`` (which sets the shutdown flag + aborts the pump
    task). It must be best-effort: callable at any lifecycle point, swallow any
    error from a torn-down engine, and idempotent (so both ``__aexit__`` and a
    signal handler can call it).
    """

    async def test_shutdown_calls_engine_stop_once(self) -> None:
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
        )
        await session.start()

        await session.shutdown()

        assert engine_registry.engine.stop_calls == 1, (
            "shutdown() must call engine.stop() exactly once"
        )

    async def test_shutdown_is_idempotent(self) -> None:
        # Mirrors the Rust stop() contract: the second call must be a no-op
        # (the handle is taken/cleared on the first), not an error. This
        # matters because __aexit__ calls shutdown() and a KeyboardInterrupt
        # handler might too.
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
        )
        await session.start()

        await session.shutdown()
        await session.shutdown()

        assert engine_registry.engine.stop_calls == 2, (
            "shutdown() is a thin delegate — idempotence is the Rust contract"
        )

    async def test_shutdown_swallows_engine_stop_exception(self) -> None:
        # A partial-startup teardown (engine torn down mid-lifecycle) must not
        # let engine.stop() mask the original in-flight exception. shutdown()
        # swallows + logs any error from stop().
        engine_registry = _FakeEngineRegistry()
        engine_registry.engine.stop_raises = RuntimeError("engine torn down")
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
        )
        await session.start()

        # Must NOT raise — the whole point of the best-effort contract.
        await session.shutdown()

        assert engine_registry.engine.stop_calls == 1, (
            "stop() was still invoked (the raise was after the call counted)"
        )

    async def test_shutdown_noop_before_start(self) -> None:
        # Callable at any lifecycle point — before start() ran,
        # engine_registry is None, so shutdown() is a quiet no-op (no
        # AttributeError). Let a Ctrl-C during startup still exit cleanly.
        session = BotRunner(
            _cfg(),
            bot=None,
            engine_registry=None,
            async_w3=None,
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
        )

        await session.shutdown()  # must not raise

    async def test_aexit_calls_shutdown_before_cancelling_consumer(self) -> None:
        # The ordering invariant: stop() the pump FIRST, then cancel the
        # consumer. Stopping the pump closes the channels → the consumer's
        # next __anext__ raises StopAsyncIteration → it ends without needing
        # the CancelledError path. Assert the call order via the event list
        # _FakeEngine/_FakeBot append to.
        events: list[str] = []
        bot = _FakeBot(events=events)
        engine_registry = _FakeEngineRegistry(events=events)

        async def hanging_consumer(**_kw):
            await asyncio.Event().wait()

        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=hanging_consumer,
        )
        await session.start()

        async with session:
            # run() will block on the hanging consumer; cancel it to exit the
            # async-with cleanly so __aexit__ runs.
            await asyncio.sleep(0.01)

        # shutdown() (→ engine.stop) ran during __aexit__, recording "stop"
        assert engine_registry.engine.stop_calls == 1
        assert "stop" in events


class TestBotRunnerSigintHandler:
    """The SIGINT→``engine.stop()`` handler closes the "first Ctrl-C swallowed"
    gap.

    During ``build_paths`` the main thread is blocked inside the synchronous
    ``find_paths`` graph prep / the Rust ``find_paths_rust`` DFS. Python's
    default SIGINT → ``KeyboardInterrupt`` is deferred until that section
    yields to the eval loop, so the first Ctrl-C appeared swallowed: the pump
    (on the shared tokio runtime) kept running and the operator pressed again.
    ``start()`` binds a handler that calls ``engine.stop()`` *immediately* the
    moment the signal arrives (the Rust DFS releases the GIL, so the handler
    runs mid-DFS). These tests verify the binding lifecycle without sending a
    real SIGINT (which is unsafe in a pytest worker).
    """

    async def test_install_binds_custom_handler_after_start(self) -> None:
        import signal

        prev = signal.getsignal(signal.SIGINT)
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
            install_sigint=True,
        )
        await session.start()

        bound = signal.getsignal(signal.SIGINT)
        assert bound is not prev, "start() must bind the custom SIGINT handler"
        assert bound != signal.SIG_DFL, "custom handler must replace the default"

    async def test_aexit_restores_previous_handler(self) -> None:
        import signal

        baseline = signal.getsignal(signal.SIGINT)
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
            install_sigint=True,
        )
        await session.start()
        assert signal.getsignal(signal.SIGINT) is not baseline

        async with session:
            await asyncio.sleep(0.01)

        # __aexit__ restored the previous handler (whatever it was — typically
        # asyncio.run's Runner handler, not SIG_DFL).
        assert signal.getsignal(signal.SIGINT) is baseline, (
            "__aexit__ must restore the SIGINT handler to the pre-start value"
        )

    async def test_install_sigint_false_does_not_bind(self) -> None:
        import signal

        baseline = signal.getsignal(signal.SIGINT)
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
            install_sigint=False,
        )
        await session.start()

        # Handler untouched — tests use this to avoid process-global pollution.
        assert signal.getsignal(signal.SIGINT) is baseline, (
            "install_sigint=False must not bind a handler"
        )

    async def test_handler_closure_calls_engine_stop(self) -> None:
        # The bound handler, when invoked, must call engine.stop() so the pump
        # is killed the instant SIGINT arrives — not deferred until find_paths
        # yields. We invoke the handler directly (not via OS signal, which is
        # unsafe under pytest) — same callable Python would call.
        import signal

        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
            install_sigint=True,
        )
        await session.start()

        handler = signal.getsignal(signal.SIGINT)
        assert handler is not signal.SIG_DFL

        # Invoking the handler must stop the engine. It also re-raises
        # KeyboardInterrupt (mirroring the default handler) — that's the
        # mechanism by which `await session.run()` unwinds to __aexit__.
        with pytest.raises(KeyboardInterrupt):
            handler(signal.SIGINT, None)

        assert engine_registry.engine.stop_calls == 1, (
            "the SIGINT handler must call engine.stop() so the pump dies "
            "immediately, even while the main thread is blocked in find_paths"
        )


class TestConstructionContext:
    """Sub-A seam: `ConstructionContext` bundles the registration-owned
    construction resources (Rust-build Bot entry + the three V3 trackers + a
    retained DB read handle + chain_id + WETH) so a background registration
    task owns them out of run()'s main-loop trim."""

    def test_for_bot_builds_trackers_weth_db_once(self) -> None:
        from degenbot.runner._driver_constants import (
            PANCAKESWAP_V3_MAINNET_FACTORY,
            SUSHISWAP_V3_MAINNET_FACTORY,
            UNISWAP_V3_MAINNET_FACTORY,
            WETH_ADDRESS,
        )
        from degenbot.runner.build_paths import ConstructionContext

        class _BuildBot:
            def __init__(self) -> None:
                self.chain_id = 1
                self.db = object()
                self.factory_addresses: list[str] = []
                self.weth_addresses: list[str] = []

            def add_tracker(self, _tracker_cls, *, factory_address, snapshot):
                self.factory_addresses.append(factory_address)
                return f"tracker:{factory_address}"

            def build_erc20token(self, address: str):
                self.weth_addresses.append(address)
                return f"weth:{address}"

        bot = _BuildBot()
        ctx = ConstructionContext.for_bot(bot, v3_snapshot=None)

        # All three V3 factories dispatched to the tracker builder.
        assert sorted(bot.factory_addresses) == sorted([
            UNISWAP_V3_MAINNET_FACTORY,
            SUSHISWAP_V3_MAINNET_FACTORY,
            PANCAKESWAP_V3_MAINNET_FACTORY,
        ])
        # Trackers + WETH + DB + chain_id all bundled into the single context.
        assert ctx.chain_id == 1
        assert ctx.db is bot.db
        assert ctx.weth == f"weth:{WETH_ADDRESS}"
        # One construction pass: exactly one WETH token requested.
        assert bot.weth_addresses == [WETH_ADDRESS]

    def test_run_passes_context_only_for_real_build_paths(self) -> None:
        """With an injected (fake) path_builder, run() must NOT build a
        context (fakes lack the builder surface) and passes context=None."""
        engine_registry = _FakeEngineRegistry()
        seen: dict = {}

        async def recording_path_builder(**kwargs):
            await asyncio.sleep(0)
            seen["kwargs"] = dict(kwargs)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=recording_path_builder,
            consumer=lambda **_kw: _noop_coro(),
        )
        asyncio.run(_drive_run(session))

        # Injected builder => run() must NOT construct a context, and must pass
        # context=None to the injected builder.
        assert session._registration_context is None
        assert "context" in seen["kwargs"]
        assert seen["kwargs"]["context"] is None
        # The builder still receives the other construction kwargs.
        assert seen["kwargs"]["engine_registry"] is engine_registry


async def _drive_run(session: BotRunner) -> None:
    await session.start()
    await session.run()


class TestSubBBackgroundRegistration:
    """Sub-B: run() decouples discovery from the main loop — production spawns
    the registration pipeline as a background task with a cross-task fail-fast
    channel, and the state-trim runs on registration completion (not on the
    main-loop entry path)."""

    async def test_background_fatal_verification_fail_fast(self) -> None:
        """A fatal verification error in the background registration cancels the
        main-loop consumer and re-raises loudly (never silently swallowed)."""
        from degenbot.exceptions import VerificationMismatchError

        engine_registry = _FakeEngineRegistry()

        async def hanging_consumer(**_kwargs):
            # Main loop that never completes on its own — only cancellation
            # (from the fail-fast channel) can end it.
            await asyncio.Event().wait()

        async def raising_path_builder(**_kwargs):
            await asyncio.sleep(0)
            boom = "tick data mismatch"
            raise VerificationMismatchError(boom)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=raising_path_builder,
            consumer=hanging_consumer,
            background_registration=True,
        )
        await session.start()
        with pytest.raises(VerificationMismatchError, match="tick data mismatch"):
            await session.run()

        # Fail-fast cancelled the main-loop consumer so the session can't keep
        # trading on unverified state.
        assert session._result_consumer_task is not None
        assert session._result_consumer_task.cancelled()

    async def test_background_run_trims_after_completion(self) -> None:
        """The state-trim runs after the background registration completes, not
        on the main-loop entry path (so it cannot clobber the shared registries
        the still-running registration reads)."""
        bot = _FakeBot()
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            background_registration=True,
        )
        session.bot = bot  # start()/run() resolve these; call the seam directly
        session.engine_registry = engine_registry

        calls: list[object] = []

        async def recording_path_builder(**kwargs):
            await asyncio.sleep(0)
            calls.append(kwargs["context"])

        await session._run_registration_background(
            path_builder=recording_path_builder,
            registration_context=None,
            retry_policy=None,
        )

        # Builder ran, then the trim executed (bot released + dropped).
        assert calls == [None]
        assert bot.released is True
        assert session.bot is None

    async def test_background_completion_closes_snapshot_tx(self) -> None:
        """A *healthy* (non-cancelled) registration must still close the
        snapshot read-tx after `build_paths` completes — the XEANMB canary stays
        active in the normal path (WAL reclamation preserved)."""
        calls: list[str] = []
        bot = _FakeBot(events=calls)
        bot._py_bot = _RecordingPyBot(calls)  # records `close_snapshot_tx`
        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=_FakeEngineRegistry(),
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            background_registration=True,
        )
        session.bot = bot
        session.engine_registry = _FakeEngineRegistry()

        async def noop_path_builder(**kwargs):
            await asyncio.sleep(0)

        await session._run_registration_background(
            path_builder=noop_path_builder,
            registration_context=None,
            retry_policy=None,
        )

        # The successful run commits the read tx (canary fires) + releases the bot.
        assert calls.count("close_snapshot_tx") == 1
        assert bot.released is True
        assert session.bot is None

    async def test_background_cancel_skips_close_snapshot_tx(self) -> None:
        """A mid-registration cancel must NOT close the snapshot read-tx.

        Registration offloads `assemble_*_tick_map` (which clone the
        `Arc<SnapshotDb>`) onto a ThreadPoolExecutor; on cancel those worker
        threads may still hold their clones, so `close_snapshot_tx`'s
        `Arc::try_unwrap` canary false-positives and would raise
        ``RuntimeError: SnapshotDb Arc still held`` during teardown (EZOKDR).
        The cancel branch must instead drop the Arc naturally and stay quiet —
        the teardown stays clean and CancelledError propagates unadorned.
        """
        calls: list[str] = []
        bot = _FakeBot(events=calls)
        # Simulate the in-flight-worker-clone scenario: the held `_py_bot`
        # would trip the canary if `close_snapshot_tx` were invoked mid-cancel.
        bot._py_bot = _RecordingPyBot(calls, raise_on_close=True)
        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=_FakeEngineRegistry(),
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            background_registration=True,
        )
        session.bot = bot
        session.engine_registry = _FakeEngineRegistry()

        async def hanging_path_builder(**kwargs):
            await asyncio.Event().wait()

        task = asyncio.create_task(
            session._run_registration_background(
                path_builder=hanging_path_builder,
                registration_context=None,
                retry_policy=None,
            )
        )
        await asyncio.sleep(0)  # let the builder start
        task.cancel()
        # The cancelled task re-raises CancelledError cleanly — the secondary
        # `close_snapshot_tx` RuntimeError must NOT surface in its place.
        with pytest.raises(asyncio.CancelledError):
            await task

        # The read-tx canary was deliberately skipped for the teardown path.
        assert "close_snapshot_tx" not in calls, (
            "close_snapshot_tx must not run on the mid-registration cancel path"
        )


class TestSubCBgRegistrationConcurrency:
    """Sub-C: the Sub-B background-registration orchestration must never deadlock
    or starve the main loop / dispatch / recursive verify, and the fail-fast
    channel must deliver a fatal registration error exactly once.

    Scoping note (consistent with Sub-A/Sub-B): registration runs as an asyncio
    task cooperatively interleaved with the consumer (the pump itself solves on
    its own tokio thread, independent of this loop). These tests drive the
    asyncio-facing orchestration with controllable fakes — forever-discovery,
    draining RPC-verify awaits, a growing block stream, and a fatal
    registration error — and assert the hot loop keeps progressing throughout.
    The genuine tokio-runtime parallel registration + real-pump no-deadlock
    belongs to the Sub-A2 Rust port / rolling smoke (U6TKNU)."""

    async def test_forever_registration_does_not_stall_main_loop(self) -> None:
        """Discovery that never exhausts must not stall the main loop / dispatch:
        the consumer makes full progress while registration still climbs."""
        engine_registry = _FakeEngineRegistry()
        dispatch_work: list[int] = []
        climbed = -1

        async def forever_path_builder(**_kwargs):
            nonlocal climbed
            i = 0
            while True:  # discovery never exhausts, but yields cooperatively
                climbed = i
                i += 1
                await asyncio.sleep(0)

        async def consumer(**_kwargs):
            # dispatch path — discrete work, then end the main loop
            for n in range(25):
                dispatch_work.append(n)
                await asyncio.sleep(0)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=forever_path_builder,
            consumer=consumer,
            background_registration=True,
        )
        await session.start()
        await session.run()

        # The hot loop made FULL progress while registration climbed forever —
        # discovery never blocked main-loop/dispatch work.
        assert dispatch_work == list(range(25))
        assert climbed > 0, "registration must have actually climbed"
        # run()'s finally cancelled the still-climbing registration at teardown.
        reg = session._registration_task
        assert reg is not None
        assert reg.cancelled()

    async def test_background_rpc_verify_drain_completes_without_deadlock(self) -> None:
        """A registration task draining many RPC-verify awaits and the consumer
        both complete cleanly — cooperative scheduling never deadlocks."""
        engine_registry = _FakeEngineRegistry()
        verify_steps = 0
        dispatch_work: list[int] = []

        async def draining_path_builder(**_kwargs):
            nonlocal verify_steps
            for _ in range(40):  # RPC-verify awaits (each a cooperative yield)
                verify_steps += 1
                await asyncio.sleep(0)

        async def consumer(**_kwargs):
            # hot loop does not depend on a 'final' discovery state: advance
            # dispatch while registration drains, then finish with it complete.
            # (``session._registration_task`` is already set by the time this
            # coroutine runs — run() creates the reg task before the main loop.)
            for n in range(10):
                dispatch_work.append(n)
                await asyncio.sleep(0)
            registration_task = session._registration_task
            assert registration_task is not None
            await registration_task  # returns once the drain is done

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=draining_path_builder,
            consumer=consumer,
            background_registration=True,
        )
        await session.start()
        await session.run()

        assert verify_steps == 40, "registration must have drained all verifies"
        assert dispatch_work == list(range(10))
        reg = session._registration_task
        # Clean, deadlock-free completion: registration finished without error.
        assert reg is not None
        assert reg.done()
        assert reg.exception() is None

    async def test_fail_fast_delivered_exactly_once(self) -> None:
        """A fatal registration transport error cancels the main loop and is
        delivered exactly once through the cross-task fail-fast channel — never
        swallowed, never re-raised twice."""
        from degenbot.exceptions import VerificationRpcError

        engine_registry = _FakeEngineRegistry()

        async def hanging_consumer(**_kwargs):
            await asyncio.Event().wait()

        async def raising_path_builder(**_kwargs):
            await asyncio.sleep(0)
            boom = "provider transport failure after bounded retry"
            raise VerificationRpcError(boom)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=raising_path_builder,
            consumer=hanging_consumer,
            background_registration=True,
        )
        await session.start()
        with pytest.raises(VerificationRpcError, match="provider transport"):
            await session.run()

        # Fail-fast cancelled the hot loop and the fatal was surfaced exactly
        # once (retrieved from the task, not re-raised twice).
        assert session._result_consumer_task is not None
        assert session._result_consumer_task.cancelled()
        reg = session._registration_task
        assert reg is not None
        assert reg.done()
        assert isinstance(reg.exception(), VerificationRpcError)

    async def test_registration_climbs_concurrently_with_main_loop(self) -> None:
        """Background discovery registration keeps climbing in its own task
        while the main loop runs (the recurring-verify task was removed, but the
        registration/main-loop concurrency contract remains)."""
        climbed = -1

        class _Engine:
            """Once-only block_stream + minimal engine surface."""

            def __init__(self) -> None:
                self.block_stream_calls = 0
                self.resumed = False

            def resume(self) -> None:
                self.resumed = True

            def last_processed_block(self) -> int | None:
                return 12_345

            def v2_pool_count(self) -> int:
                return 0

            def v3_pool_count(self) -> int:
                return 0

            def v4_pool_count(self) -> int:
                return 0

            def path_count(self) -> int:
                return 0

            def block_stream(self):
                self.block_stream_calls += 1
                if self.block_stream_calls > 1:
                    boom = "block_stream() can only be called once"
                    raise RuntimeError(boom)
                # Every block divisible by RECURRING_VERIFY_INTERVAL (50).
                return _BlocksStream(
                    [_block_dict(500), _block_dict(550), _block_dict(600)],
                )

        class _Registry:
            def __init__(self) -> None:
                self.engine = _Engine()

            def start(self, *_a, **_kw) -> int:
                return 0  # no backfill beyond current_block — main-loop entry

        async def forever_path_builder(**_kwargs):
            nonlocal climbed
            i = 0
            while True:
                climbed = i
                i += 1
                await asyncio.sleep(0)

        async def recording_consumer(*, block_stream=None, **_kw) -> None:
            async for _b in block_stream:
                await asyncio.sleep(0)

        registry = _Registry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=registry,  # type: ignore[arg-type]
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=forever_path_builder,
            consumer=recording_consumer,
            background_registration=True,
        )
        await session.start()
        await session.run()

        assert registry.engine.block_stream_calls == 1
        assert registry.engine.resumed is True
        assert climbed > 0, "registration must have climbed concurrently with the main loop"
        # Main loop ended on the finite block stream; finally cancelled the
        # still-climbing registration.
        assert session._registration_task is not None
        assert session._registration_task.cancelled()


class Test6VZN7HOngoingDiscovery:
    """6VZN7H: the run()-level wiring when discovery never "completes".

    The unbounded production discovery producer (``_discovery_producer_forever``
    re-sweeping the subgraph) was stripped back to a single discovery pass, so
    the producer-level tests are gone. What remains is the run()-level
    guarantee: with a never-returning injected path builder there is no
    "registration completion", so the Sub-B state-trim runs when run() cancels
    the background task at shutdown — still exactly once and not mid-climb."""

    async def test_forever_discovery_trims_state_on_shutdown(self) -> None:
        """With forever (never-returning) discovery there is no "registration
        completion", so the Sub-B state-trim runs when run() cancels the
        background task at shutdown instead — still exactly once and not
        mid-climb."""
        engine_registry = _FakeEngineRegistry()
        bot = _FakeBot()

        async def forever_path_builder(**_kwargs):
            await asyncio.Event().wait()  # never completes on its own

        async def consumer(**_kwargs):
            for _ in range(3):
                await asyncio.sleep(0)

        session = BotRunner(
            _cfg(),
            bot=bot,
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=forever_path_builder,
            consumer=consumer,
            background_registration=True,
        )
        await session.start()
        await session.run()

        # Main loop ended (finite consumer) → run()'s finally cancelled the
        # forever registration → trim ran on cancellation (shutdown-time).
        assert bot.released is True
        reg = session._registration_task
        assert reg is not None
        assert reg.cancelled()


class TestPathRegistrationPipeline:
    """NWTUM3 S1: the reusable, pump-concurrent `PathRegistrationPipeline`.

    Covers the operator-facing surface that discovery previously ran inline:
    enqueue one specific path (`enqueue_path`), bounded on-demand discovery
    (`trigger_discovery`), registered-path dedup shared with workers, and the
    preserved fail-fast tripwire. All work is driven through the SAME `_consume`
    body the discovery workers use, so behavior cannot diverge by input source.
    """

    # Local fakes — the real construction/engine surfaces are injected so these
    # tests assert the pipeline's routing/seam, not live RPC/DB/verify.
    @dataclass
    class _FakePool:
        address: str

    class _FakeCtxBot:
        def __init__(self) -> None:
            self.chain_id = 1
            self.db = object()

        def build_pool(self, address: str, **kwargs: object):
            return TestPathRegistrationPipeline._FakePool(address)

    class _FakeReg:
        def __init__(self) -> None:
            self.register_path_calls = 0

        def register_v2_pool(self, pool: object) -> None:
            pass

        def register_path(self, zipped: object) -> None:
            self.register_path_calls += 1

    @dataclass
    class _Step:
        type: object
        address: str
        hash: object | None = None

    @staticmethod
    def _make_pipeline(fail_on_register: Exception | None = None):
        from degenbot.database.models.pools import UniswapV2PoolTableBase
        from degenbot.runner.build_paths import (
            ConstructionContext,
            PathRegistrationPipeline,
        )

        bot = TestPathRegistrationPipeline._FakeCtxBot()

        class _Reg(TestPathRegistrationPipeline._FakeReg):
            def register_v2_pool(self, pool: object) -> None:
                if fail_on_register is not None:
                    raise fail_on_register

        reg = _Reg()
        weth = type("_Weth", (), {"address": "0x" + "1" * 40})()
        ctx = ConstructionContext(
            bot=bot,
            chain_id=1,
            db=bot.db,
            uniswap_v3_tracker=object(),
            sushiswap_v3_tracker=object(),
            pancakeswap_v3_tracker=object(),
            weth=weth,
        )
        pipeline = PathRegistrationPipeline(context=ctx, engine_registry=reg)
        # The pipeline retains its own context (NWTUM3 trimmed-state guarantee):
        # a call-site that drops run()'s bot (and even the local `ctx` ref)
        # still has everything construction needs.
        assert pipeline.constr_bot is bot
        return pipeline, reg, UniswapV2PoolTableBase

    async def test_enqueue_path_registers_single_path(self) -> None:
        pipeline, reg, t_base = self._make_pipeline()
        step = self._Step(t_base, "0x" + "a" * 40)
        await pipeline.enqueue_path([step], directions=[True])

        # One explicit path registered through the shared consume body.
        assert pipeline.path_count == 1
        assert reg.register_path_calls == 1
        # Dedup set is populated so a repeat add is rejected as a duplicate.
        assert len(pipeline.registered_path_sigs) == 1

    async def test_enqueue_path_dedups_repeat(self) -> None:
        pipeline, reg, t_base = self._make_pipeline()
        step = self._Step(t_base, "0x" + "b" * 40)
        await pipeline.enqueue_path([step], directions=[True])
        await pipeline.enqueue_path([step], directions=[True])

        # Second add of the identical (pools, directions) path is a duplicate,
        # not a second registration.
        assert pipeline.path_count == 1
        assert reg.register_path_calls == 1
        assert pipeline.dup_count == 1

    async def test_trigger_discovery_bounded_feeds_shared_consume(self) -> None:
        pipeline, _reg, _t_base = self._make_pipeline()
        consumed: list[object] = []

        async def counting_consume(item, directions=None):
            await asyncio.sleep(0)
            consumed.append(item)

        pipeline._consume = counting_consume  # type: ignore[method-assign]

        async def sweep():
            for item in ["p0", "p1", "p2"]:  # type: ignore[list-item]
                await asyncio.sleep(0)
                yield item

        pipeline.discovery_sweep = sweep  # type: ignore[method-assign]

        n = await pipeline.trigger_discovery(bound=2)
        # Bounded: only 2 of the 3 sweep items consumed; same `_consume` seam.
        assert n == 2
        assert consumed == ["p0", "p1"]

    async def test_enqueue_path_preserves_fail_fast_tripwire(self) -> None:
        from degenbot.exceptions import VerificationMismatchError

        pipeline, _reg, t_base = self._make_pipeline(
            fail_on_register=VerificationMismatchError("boom")
        )
        step = self._Step(t_base, "0x" + "c" * 40)
        # A fatal verification mismatch is NOT swallowed by enqueue_path — it
        # propagates so the caller can abort loudly (crash-loudly preserved).
        with pytest.raises(VerificationMismatchError, match="boom"):
            await pipeline.enqueue_path([step], directions=[True])

    async def test_forever_discovery_plus_mid_run_add_compose_without_stall(self) -> None:
        """U6TKNU terminal composition: the unbounded forever discovery producer
        and a mid-run operator path-add flow through the SAME live pipeline's
        `_consume` concurrently — the add registers while forever discovery
        keeps climbing, and neither stalls the other (cooperative scheduling
        over the shared body, no deadlock). This is the epic's end-to-end core:
        solve/registration work for newly-added paths proceeds alongside
        ongoing discovery that never reaches a terminal state.

        A 1-hop V2 path with explicit `directions=[True]` is used so the add
        fully registers through the real `_consume` offline (no live RPC
        token-direction resolution needed). Forever discovery yields deliberately
        opaque path shapes (a step whose type is not a pool table class) that
        `_consume` skips (counted as `skip_count`), exercising the pipeline body
        + backpressure forever without live construction — and, crucially,
        WITHOUT aborting the pipeline (a raw ``object()`` would make `_consume`
        do `list(object())` → TypeError, masking the real composition).
        """
        from degenbot.runner._driver_constants import REG_QUEUE_BOUND, REG_WORKERS
        from degenbot.runner.build_paths import run_registration_pipeline

        pipeline, reg, t_base = self._make_pipeline()
        prior_skips = pipeline.skip_count

        async def forever_producer():
            i = 0
            while True:
                # Opaque path shape `_consume` skips (step type = `object`, not
                # a V2/V3/V4 pool table class) — exercises the pipeline body +
                # backpressure forever, and safe (no list(object()) TypeError).
                i += 1
                await asyncio.sleep(0)
                yield [self._Step(type=object, address="0x" + f"{i:x}" * 40)]

        # Run the unbounded discovery producer through the pipeline's bounded
        # producer/consumer as a background task (never returns).
        reg_task = asyncio.create_task(
            run_registration_pipeline(
                producer=forever_producer(),
                consume=pipeline._consume,
                queue_size=REG_QUEUE_BOUND,
                worker_count=REG_WORKERS,
            )
        )
        try:
            # Let forever discovery climb a few cooperative steps.
            for _ in range(5):
                await asyncio.sleep(0)

            # MID-RUN add: a concrete 1-hop V2 path through the SAME `_consume`,
            # while forever discovery is still climbing in the background task.
            step = self._Step(t_base, "0x" + "d" * 40)
            await pipeline.enqueue_path([step], directions=[True])

            # The add registered (not stalled by ongoing discovery).
            assert reg.register_path_calls == 1
            assert pipeline.path_count == 1
            # Forever discovery kept climbing concurrently (no stall/deadlock) —
            # it drove real `_consume` skips beyond the pre-add baseline.
            assert pipeline.skip_count > prior_skips, (
                "forever discovery must have progressed through _consume"
            )
            # The background pipeline task is still alive (never returned) —
            # proof endless discovery does not reach a terminal state while the
            # mid-run add proceeds.
            assert reg_task.done() is False
        finally:
            reg_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await reg_task

    async def test_periodic_progress_surfaces_skip_reasons_below_1000(
        self, caplog: pytest.LogCaptureFixture
    ) -> None:
        """INN6TK observability: the registration counters + a top skip-reason
        breakdown must be visible even when ``path_count < 1000``. The legacy
        ``[build_paths] Progress`` line only fires at ``path_count % 1000 == 0``,
        so a discovery-heavy crawl that registers few paths (INN6TK: 12M
        discovered, <1000 registered) never prints it and the skip/dup/reject
        reasons stay invisible. The new time-based summary must surface them.
        """
        import logging

        pipeline, _reg, _t_base = self._make_pipeline()
        # Simulate a crawl that is skipping almost everything well below the
        # 1000-registration print threshold.
        pipeline._record_skip("build-v3:ConnectionError")
        pipeline._record_skip("build-v3:ConnectionError")
        pipeline._record_skip("direction-fail")
        pipeline.path_count = 7
        pipeline.skip_count = 3

        with caplog.at_level(logging.INFO):
            pipeline.emit_registration_progress(force=True)

        msg = "\n".join(r.getMessage() for r in caplog.records)
        assert "[build_paths] Progress" in msg
        assert "7 paths registered" in msg
        assert "3 skipped" in msg
        # Top skip-reason breakdown (rounded down to the most-common) is present.
        assert "build-v3:ConnectionError=2" in msg
        assert "direction-fail=1" in msg


class TestSessionOperatorSurface:
    """NWTUM3: the programmatic add-a-path-at-any-time surface exposed on the
    session (`BotRunner.enqueue_path` / `trigger_discovery`), which routes
    into the long-lived `PathRegistrationPipeline`. These inject a fake pipeline
    (consistent with the suite's fake-injection pattern); the pipeline-level
    `_consume` registration/isolation behaviour is covered by
    `TestPathRegistrationPipeline`."""

    async def test_programmatic_surface_raises_without_live_pipeline(self) -> None:
        """With an injected (fake) run there is no live pipeline — the operator
        surface must raise loudly, never fail silently or touch a None."""
        engine_registry = _FakeEngineRegistry()
        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=lambda **_kw: _noop_coro(),
        )
        await session.start()
        await session.run()
        assert session._pipeline is None
        with pytest.raises(RuntimeError, match="no live registration pipeline"):
            await session.enqueue_path([])
        with pytest.raises(RuntimeError, match="no live registration pipeline"):
            await session.trigger_discovery()

    async def test_mid_run_add_path_does_not_stall_dispatch(self) -> None:
        """Adding a path mid-run via the session Programmatic surface does not
        stall or abort the pump: the consumer keeps dispatching prior work while
        the operator add is routed to the live pipeline."""
        engine_registry = _FakeEngineRegistry()
        dispatch_work: list[int] = []
        added: list[tuple[object, object]] = []

        class _FakePipeline:
            def __init__(self) -> None:
                self.enqueue_calls = 0
                self.trigger_calls = 0

            async def enqueue_path(self, path_steps, directions=None):
                self.enqueue_calls += 1
                added.append((path_steps, directions))

            async def trigger_discovery(self, bound=None):
                self.trigger_calls += 1
                return int(bound or 0)

        fake_pipeline = _FakePipeline()

        async def consumer(**_kwargs):
            for n in range(20):
                dispatch_work.append(n)
                if n == 7:
                    # Operator adds a path + triggers discovery mid-run while
                    # the hot loop keeps solving previously-added hops.
                    await session.enqueue_path(["hop-a", "hop-b"], directions=[True, False])
                    await session.trigger_discovery(bound=3)
                await asyncio.sleep(0)

        session = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=engine_registry,
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=lambda **_kw: _noop_coro(),
            consumer=consumer,
        )
        # A live (fake) pipeline is reachable on the running session.
        session._pipeline = fake_pipeline
        await session.start()
        await session.run()

        # Full dispatch progress despite the mid-run add — no stall, no abort.
        assert dispatch_work == list(range(20))
        assert fake_pipeline.enqueue_calls == 1
        assert fake_pipeline.trigger_calls == 1
        assert added == [(["hop-a", "hop-b"], [True, False])]
