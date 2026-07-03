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


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Set chain-1 RPC OS envvars for every test in this module.

    The session tests inject fakes for bot/engine_registry/async_w3, so the RPC
    URIs are never connected — they only need to be *present* on the config so
    ``BackrunConfig.from_env`` does not raise ``RpcNotConfiguredError``. Keeping
    them ``http://localhost:8545`` / ``ws://localhost:8546`` preserves the legacy
    assertions in ``test_start_orchestrates_pre_resume_ritual``.
    """
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    """Restore the default SIGINT handler after each test.

    ``BackrunSession.start()`` binds a SIGINT→``engine.stop()`` handler on the
    production path (``install_sigint=True``). Tests that call ``start()`` then
    ``run()`` directly (without ``async with``) never reach ``__aexit__``, so
    the handler would leak across tests and pollute ``signal.getsignal``
    assertions. This fixture restores ``SIG_DFL`` after each test unconditionally.
    """
    yield
    import signal as _signal

    _signal.signal(_signal.SIGINT, _signal.SIG_DFL)


def _cfg(**overrides) -> BackrunConfig:
    base = {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
    }
    base.update(overrides)
    return BackrunConfig.from_env(base, live=True, permutation=None)


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
        self.verify_liquidity_maps_calls: int = 0  # step 3b batch verify count

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

    async def verify_liquidity_maps(self, *, block_number=None) -> None:
        self.verify_liquidity_maps_calls += 1
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
    """Fake ``AsyncProviderAdapter`` for BackrunSession tests (PAGQCK).

    The dispatch hot loop was routed off raw ``AsyncWeb3`` onto
    ``AsyncProviderAdapter`` — this fake exposes the SAME flat surface
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

    def __aiter__(self) -> "_BlocksStream":
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

    async def test_run_skips_redundant_startup_batch_verify(self) -> None:
        """Step 3b (the startup batch `verify_liquidity_maps()` at the moving
        head) is redundant with the per-pool two-step verify: step-1 (seed)
        proves the snapshot was good; step-2 (post-drain) proves the drain/pump
        applied events correctly. Re-verifying at `last_processed_block()`
        also races the pump's WS log-application lag — a block's header can
        advance `last_processed_block` past it before its Mint log is
        dispatched (V2-V2-V3 crash: Mint at 25397047 unapplied when 25397049's
        header advanced the cursor). The per-pool gates (step-1/step-2) are
        race-free and run inside `build_paths`; this batch gate must NOT run.
        """
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

        assert engine_registry.verify_liquidity_maps_calls == 0, (
            "run() must NOT call the batch verify_liquidity_maps (step 3b) — "
            "redundant with the per-pool step-1/step-2 gates and racy at the "
            "moving head"
        )


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


class TestBackrunSessionRunBlockStreamAcquiredOnce:
    """Regression: `run()` must acquire `engine.block_stream()` exactly once and
    fan the blocks to both the result consumer and the T7 recurring-verify
    ticker via `_tee_block_stream`.

    Pre-fix `run()` called `engine.block_stream()` twice: once consumed inside
    the real `consume_result_batches` (which self-acquires when
    `block_stream=None`), and once at run() line 967 for the recurring-verify
    ticker. The real `PyUniswapArbEngine.block_stream()` is once-only — the
    second call raised `RuntimeError("block_stream() can only be called once")`
    entering the main loop, crashing every permutation run. This test uses an
    engine whose `block_stream()` raises on the second call (mimicking the real
    once-only seam) and asserts both branches receive every block.
    """

    async def test_run_acquires_block_stream_once_and_fans_to_both_consumers(
        self,
    ) -> None:
        seen_by_consumer: list[int] = []
        seen_by_verify: list[int] = []

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
                    raise RuntimeError("block_stream() can only be called once")
                # Blocks divisible by RECURRING_VERIFY_INTERVAL (50) so the
                # recurring-verify ticker actually fires at each.
                return _BlocksStream(
                    [_block_dict(500), _block_dict(550), _block_dict(600)],
                )

        class _Registry:
            def __init__(self) -> None:
                self.engine = _OnceOnlyEngine()

            def start(self, node_http, node_ws, *, v3_snapshot, v4_snapshot, verify_state_view) -> int:
                # No backfill target beyond current_block (12_345) — main-loop entry.
                return 0

            async def verify_liquidity_maps(self, *, block_number=None) -> None:
                seen_by_verify.append(block_number)

        registry = _Registry()

        async def recording_consumer(*, block_stream=None, **_kw) -> None:
            async for b in block_stream:
                seen_by_consumer.append(b["number"])

        session = BackrunSession(
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
        # Both branches received every block (fan-out, not a split).
        assert seen_by_consumer == [500, 550, 600], (
            "result-consumer branch must receive every block"
        )
        # The recurring-verify ticker fires at each block divisible by
        # RECURRING_VERIFY_INTERVAL (50). (Pre-removal, a leading `None` from
        # the now-removed step-3b startup batch verify_liquidity_maps() appeared
        # here — that gate is redundant with the per-pool step-1/step-2 gates
        # and racy at the moving head, so it no longer runs.)
        assert seen_by_verify == [500, 550, 600], (
            "recurring-verify ticker must fire at each interval block"
        )


class TestBackrunSessionShutdown:
    """``BackrunSession.shutdown()`` is the clean-shutdown seam that hands a
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
        session = BackrunSession(
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
        session = BackrunSession(
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
        session = BackrunSession(
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
        session = BackrunSession(
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

        session = BackrunSession(
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


class TestBackrunSessionSigintHandler:
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
        session = BackrunSession(
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
        session = BackrunSession(
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
        session = BackrunSession(
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
        session = BackrunSession(
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
