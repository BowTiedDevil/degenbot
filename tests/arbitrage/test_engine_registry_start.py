"""Integration tests for EngineRegistry.start — the two-phase Layer A facade.

`start(...)` runs the pre-pump startup ritual (subscribe → stream snapshots →
backfill → verify config) and stops at `EnginePhase::Backfilled`, BEFORE
`resume()`. This is the consumer-safety invariant: between subscribe and
resume (including stream/backfill/verify) zero result batches are emitted,
so the caller can attach its consumer any time before `resume()` without
risk of unbounded backlog or stale-batch dispatch.
"""

from __future__ import annotations

import asyncio
import inspect

import pytest

import degenbot.arbitrage.engine_registry as runner


class FakeEngine:
    """Records Layer A lifecycle calls in order; never resumes.

    Stubs the narrow surface `start()` touches: subscribe, the stream fn's
    non-DB fallback (`load_v3_snapshot_from_py`), backfill, and the verify
    setters. `resume()` is intentionally a recordable call so tests can assert
    it was NOT invoked.
    """

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.backfill_args: list[tuple[str, int]] = []
        self.verify_args: dict | None = None
        self._last_processed_block: int | None = 18_000_042

    def subscribe(self, ws: str) -> int:
        self.calls.append("subscribe")
        return 18_000_000  # backfill_target

    def load_v3_snapshot_from_py(self, snapshot: object) -> None:
        self.calls.append("stream_v3")

    def load_v4_snapshot_from_py(self, snapshot: object) -> None:
        self.calls.append("stream_v4")

    def backfill_from_snapshot(self, rpc: str, snapshot_block: int) -> int:
        self.calls.append("backfill")
        self.backfill_args.append((rpc, snapshot_block))
        return 0

    def set_verify_rpc_url(self, rpc: str) -> None:
        self.calls.append("set_verify_rpc_url")

    def set_verify_state_view(self, addr: str) -> None:
        self.calls.append("set_verify_state_view")

    def last_processed_block(self) -> int | None:
        return self._last_processed_block

    def verify_liquidity_maps(
        self,
        *,
        rpc_url: str,
        tick_lens_address: str,
        state_view_address: str,
        block_number: int | None,
    ) -> None:
        # Mirrors the real engine seam: async (driven by asyncio via
        # `future_into_py`), so the registry `await`s it. A plain sync return
        # here would make the registry's `await` raise — keep it async.
        async def _verify() -> None:
            self.calls.append("verify_liquidity_maps")
            self.verify_args = {
                "rpc_url": rpc_url,
                "tick_lens_address": tick_lens_address,
                "state_view_address": state_view_address,
                "block_number": block_number,
            }

        return _verify()  # coroutine — registry `await`s it

    def resume(self) -> None:
        self.calls.append("resume")


def test_start_no_snapshots_calls_subscribe_then_verify_never_resume() -> None:
    """Tracer: with no snapshots, start() subscribes + sets verify config in
    order and never calls resume(). Skip stream/backfill (no snapshots)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    backfill_target = registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
    )

    assert fake.calls == [
        "subscribe",
        "set_verify_rpc_url",
    ]
    assert "resume" not in fake.calls
    assert backfill_target == 18_000_000


class _FakeSnapshot:
    """Minimal snapshot double exposing ``newest_block`` for block derivation."""

    def __init__(self, newest_block: int) -> None:
        self.newest_block = newest_block


def test_start_derives_snapshot_block_as_min_newest_block(monkeypatch) -> None:
    """start() derives snapshot_block = min(snap.newest_block) across supplied
    snapshots and passes it to backfill_from_snapshot — never a user param.

    The module-level stream functions are patched with recorders (the real fns
    need a full DB-backed snapshot); the engine is a real Fake. This verifies
    start()'s ORCHESTRATION — that it calls stream then backfill with the
    derived block in the documented order — not the stream fns themselves.
    """
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    def record_v3_stream(snapshot, engine) -> None:
        assert engine is fake
        fake.calls.append("stream_v3")

    def record_v4_stream(snapshot, engine) -> None:
        assert engine is fake
        fake.calls.append("stream_v4")

    monkeypatch.setattr(runner, "stream_v3_snapshot_to_engine", record_v3_stream)
    monkeypatch.setattr(runner, "stream_v4_snapshot_to_engine", record_v4_stream)

    v3_snap = _FakeSnapshot(newest_block=18_000_100)
    v4_snap = _FakeSnapshot(newest_block=18_000_050)

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=v3_snap,
        v4_snapshot=v4_snap,
    )

    # The derived block is the min of the two newest_blocks.
    assert fake.backfill_args == [("http://localhost:8545", 18_000_050)]
    # Streams ran for both snapshots, in order, then backfill, then verify.
    assert fake.calls == [
        "subscribe",
        "stream_v3",
        "stream_v4",
        "backfill",
        "set_verify_rpc_url",
    ]
    assert "resume" not in fake.calls


def test_start_passes_verify_state_view_when_supplied() -> None:
    """verify_state_view is set on the engine only when supplied."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view=state_view,
    )

    assert "set_verify_state_view" in fake.calls


def test_start_skips_set_verify_state_view_when_none() -> None:
    """When verify_state_view is None, set_verify_state_view is not called."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    assert "set_verify_state_view" not in fake.calls


def test_pybot_exposes_verify_methods_after_engine_attach() -> None:
    """T4 (ADR-006 D4): PyBot exposes set_verify_rpc_url, set_verify_state_view,
    verify_liquidity_maps, verify_v3_liquidity_maps, verify_v4_liquidity_maps as
    delegating entry points once a UniswapArbEngine is constructed against it.
    The batch verify still passes (same behavior, new home on PyBot)."""
    from degenbot.degenbot_rs import PyBot, UniswapArbEngine

    bot = PyBot()
    UniswapArbEngine(py_bot=bot)  # attaches shared PumpState
    for method in (
        "set_verify_rpc_url",
        "set_verify_state_view",
        "verify_liquidity_maps",
        "verify_v3_liquidity_maps",
        "verify_v4_liquidity_maps",
    ):
        assert hasattr(bot, method), f"PyBot must expose {method} after engine attach"


def test_pybot_exposes_pump_lifecycle_methods_after_engine_attach() -> None:
    """T3 (ADR-006 D4): PyBot exposes subscribe/backfill_from_snapshot/resume
    as delegating entry points once a UniswapArbEngine is constructed against
    it (which attaches the shared PumpState). The Bot is the D4 pump owner;
    these methods drive the SAME PumpState the engine reads. Before T3 only
    the engine had them."""
    from degenbot.degenbot_rs import PyBot, UniswapArbEngine

    bot = PyBot()
    # Constructing the engine against the bot attaches the shared PumpState.
    engine = UniswapArbEngine(py_bot=bot)
    for method in ("subscribe", "backfill_from_snapshot", "resume"):
        assert hasattr(bot, method), f"PyBot must expose {method} after engine attach"
    # The engine still exposes them too (reads the same shared state).
    for method in ("subscribe", "backfill_from_snapshot", "resume"):
        assert hasattr(engine, method)


def test_start_stashes_snapshot_and_backfill_blocks_for_two_step_verify(monkeypatch) -> None:
    """T1 (ADR-006 D4 + two-step verify prep): start() stashes the snapshot
    block (min newest_block) and the backfill target (from subscribe) on the
    registry, so the per-pool two-step verify (T6) can pass them to the verify
    closures without re-deriving. These are NOT wired into engine.set_verify_*
    (that dead path is deleted in T5) — they live on the registry for T6 to read.
    No behavior change yet beyond setting the fields (T6 reads them)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    def _noop(snapshot, engine) -> None:  # noqa: ARG001
        pass

    monkeypatch.setattr(runner, "stream_v3_snapshot_to_engine", _noop)
    monkeypatch.setattr(runner, "stream_v4_snapshot_to_engine", _noop)

    v3_snap = _FakeSnapshot(newest_block=18_000_100)
    v4_snap = _FakeSnapshot(newest_block=18_000_050)

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=v3_snap,
        v4_snapshot=v4_snap,
    )

    # snapshot_block is the min of the supplied newest_blocks; backfill_block
    # is the subscribe() return (first observed WS block).
    assert registry._verify_snapshot_block == 18_000_050
    assert registry._verify_backfill_block == 18_000_000


def test_start_stashes_None_blocks_when_no_snapshots() -> None:
    """With no snapshots, there's no snapshot block to derive and no backfill
    applied, so both block stashes are None (T6 will guard on None == verify
    not applicable for this pool)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    assert registry._verify_snapshot_block is None
    assert registry._verify_backfill_block is None


def test_verify_liquidity_maps_raises_when_start_not_called() -> None:
    """Before start() stashes verify config, verify_liquidity_maps is a
    RuntimeError — never a silent skip or an unconfigured-RPC failure deep in
    the engine. Mirrors the WFDTUR fail-fast posture."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    with pytest.raises(RuntimeError, match="verify config"):
        asyncio.run(registry.verify_liquidity_maps())
    assert "verify_liquidity_maps" not in fake.calls


def test_verify_liquidity_maps_delegates_with_stashed_config() -> None:
    """After start(..., verify_state_view=...), verify_liquidity_maps delegates
    to the engine with the stashed RPC + StateView, a zero tick_lens (unused by
    the V3 batch path), and block_number = the engine's last_processed_block()
    (NOT ``pending``/latest — engine-state@N vs chain@N is deterministic).
    Emits exactly one delegate call — the [verify] line the analyzer keys on."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view=state_view,
    )

    asyncio.run(registry.verify_liquidity_maps())

    assert fake.verify_args == {
        "rpc_url": "http://localhost:8545",
        "tick_lens_address": "0x0000000000000000000000000000000000000000",
        "state_view_address": state_view,
        "block_number": 18_000_042,
    }


def test_verify_liquidity_maps_explicit_block_overrides_default() -> None:
    """An explicit block_number wins over the engine's last_processed_block —
    lets a caller pin a specific checkpoint (e.g. the snapshot block)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view="0x0000000000000000000000000000000000000abc",
    )

    asyncio.run(registry.verify_liquidity_maps(block_number=25_384_822))

    assert fake.verify_args["block_number"] == 25_384_822


def test_verify_liquidity_maps_falls_back_to_pending_when_no_block() -> None:
    """If the engine has processed no block yet (last_processed_block() is None),
    verify falls through to ``pending`` (latest) rather than crashing — the
    engine state is empty so a latest-check is the only honest comparison."""
    fake = FakeEngine()
    fake._last_processed_block = None
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view="0x0000000000000000000000000000000000000abc",
    )

    asyncio.run(registry.verify_liquidity_maps())

    assert fake.verify_args["block_number"] is None


def test_verify_liquidity_maps_is_async_driven_by_asyncio_not_block_on() -> None:
    """verify_liquidity_maps MUST be an async function (driven by asyncio via
    pyo3 `future_into_py`), NOT a sync `runtime.block_on` on the shared
    multi-threaded tokio runtime.

    Regression guard for the 2026-06-24 deadlock: `run()` called the sync
    `engine.verify_liquidity_maps` (pyo3) which did `runtime.block_on(...)`
    from a foreign (Python) thread while the pump WS loop was a task on that
    same runtime. Under the pump's concurrent lock churn the verify future's
    worker couldn't get scheduled → MainThread + all tokio workers parked on
    `futex_do_wait` (proven via py-spy + wchan) → permanent stall, no
    `[DIAG] HEADER` and no `[verify] ... OK`. The async seam (driven by the
    asyncio loop the consumer already runs on) eliminates the foreign-thread
    `block_on`, so verify runs concurrently with the pump and cannot deadlock.
    """
    assert inspect.iscoroutinefunction(runner.EngineRegistry.verify_liquidity_maps), (
        "verify_liquidity_maps must be async — sync block_on deadlocks the pump "
        "(see 2026-06-24 diagnosis)."
    )


def test_verify_liquidity_maps_raises_when_state_view_omitted() -> None:
    """V4 batch verify needs StateView; start() without verify_state_view must
    surface as a config error at verify time, not a silent pass."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start("http://localhost:8545", "ws://localhost:8546")  # no state_view

    with pytest.raises(RuntimeError, match="verify config"):
        asyncio.run(registry.verify_liquidity_maps())
