"""Integration tests for EngineRegistry.start — the two-phase Layer A facade.

`start(...)` runs the pre-pump startup ritual (subscribe → stream snapshots →
backfill → verify config) and stops at `EnginePhase::Backfilled`, BEFORE
`resume()`. This is the consumer-safety invariant: between subscribe and
resume (including stream/backfill/verify) zero result batches are emitted,
so the caller can attach its consumer any time before `resume()` without
risk of unbounded backlog or stale-batch dispatch.
"""

from __future__ import annotations

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
        # JUCFCB: the DB path reads `snapshot_seed_block` from the engine.
        # FakeEngine defaults to None (cold-start → no backfill) unless a test
        # sets it.
        self._snapshot_seed_block: int | None = None
        # 2SM4Y7: the non-DB path now records S via `set_snapshot_seed_block`
        # (the pyo3 `backfill_from_snapshot` retired); the FakeEngine records
        # the call so tests can assert the non-DB path drives the seed-set.
        self.seed_args: list[int | None] = []

    @property
    def snapshot_seed_block(self) -> int | None:
        return self._snapshot_seed_block

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, value: int | None) -> None:
        self.calls.append("set_snapshot_seed_block")
        self.seed_args.append(value)
        self._snapshot_seed_block = value

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
    snapshots and stashes it for the per-pool verify, never passing it to
    a backfill call (J3FMDO: the snapshot→WS gap closes automatically inside
    `resume()` via the core `BlockPump::resume_from_subscribe`, not from
    `start()`).

    The module-level stream functions are patched with recorders (the real fns
    need a full DB-backed snapshot); the engine is a real Fake. This verifies
    start()'s ORCHESTRATION — that it streams, stashes the derived block as
    `_verify_snapshot_block`, and configures verify in the documented order —
    not the stream fns themselves.
    """
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    # XEANMB: `start()` no longer ingests snapshot dicts (the
    # `load_*_from_py` surface is retired); it derives `S = min(newest_block)`
    # + sets `snapshot_seed_block` BEFORE `subscribe()` so `after_subscribe`
    # advances the phase to `SnapshotLoaded`.
    v3_snap = _FakeSnapshot(newest_block=18_000_100)
    v4_snap = _FakeSnapshot(newest_block=18_000_050)

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=v3_snap,
        v4_snapshot=v4_snap,
    )

    # J3FMDO: start() no longer calls backfill_from_snapshot — resume() drives
    # it via the core auto-backfill. So backfill_args stays empty.
    assert fake.backfill_args == []
    # XEANMB: the snapshot seed block is set BEFORE subscribe (so the engine
    # phase advances to SnapshotLoaded via after_subscribe), then
    # verify-config. No stream/load_*_from_py calls remain.
    assert fake.calls == [
        "set_snapshot_seed_block",
        "subscribe",
        "set_verify_rpc_url",
    ]
    # The derived block (the min of the two newest_blocks) is set on the
    # engine's snapshot_seed_block + stashed as the step-1 verify seed.
    assert fake.seed_args == [18_000_050]
    assert registry._verify_snapshot_block == 18_000_050
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
    """T4 (ADR-006 D4): Bot exposes the verify CONFIG + per-pool two-step
    lifecycle entry points once an ArbitrageEngine is constructed against it.
    The whole-batch `verify_liquidity_maps` (and V3/V4 twins) was REMOVED as
    redundant + racy (the per-pool two-step lifecycle is the verify authority),
    so those must NOT be present."""
    from degenbot._ffi import Bot
    from degenbot.arbitrage.engine_registry import ArbitrageEngine

    bot = Bot()
    ArbitrageEngine(py_bot=bot)  # attaches shared PumpState
    for method in (
        "set_verify_rpc_url",
        "set_verify_state_view",
    ):
        assert hasattr(bot, method), f"Bot must expose {method} after engine attach"
    for method in (
        "verify_liquidity_maps",
        "verify_v3_liquidity_maps",
        "verify_v4_liquidity_maps",
    ):
        assert not hasattr(bot, method), f"removed batch verify {method} must not exist"


def test_pybot_exposes_pump_lifecycle_methods_after_engine_attach() -> None:
    """T3 (ADR-006 D4): Bot exposes subscribe/resume as delegating entry
    points once a ArbitrageEngine is constructed against it (which attaches
    the shared PumpState). The Bot is the D4 pump owner; these methods drive
    the SAME PumpState the engine reads.

    2SM4Y7: `backfill_from_snapshot` is retired — the snapshot→WS gap is
    closed automatically inside the core `BlockPump::resume_from_subscribe`
    (J3FMDO). The non-DB path uses the `snapshot_seed_block` setter to record
    `S` so the core auto-backfill picks it up.
    """
    from degenbot._ffi import Bot
    from degenbot.arbitrage.engine_registry import ArbitrageEngine

    bot = Bot()
    # Constructing the engine against the bot attaches the shared PumpState.
    engine = ArbitrageEngine(py_bot=bot)
    for method in ("subscribe", "resume"):
        assert hasattr(bot, method), f"Bot must expose {method} after engine attach"
    # 2SM4Y7: backfill_from_snapshot is retired.
    assert not hasattr(bot, "backfill_from_snapshot"), (
        "Bot::backfill_from_snapshot retired (2SM4Y7)"
    )
    # The engine still exposes subscribe/resume too (reads the same shared state).
    for method in ("subscribe", "resume"):
        assert hasattr(engine, method)
    assert not hasattr(engine, "backfill_from_snapshot"), (
        "ArbitrageEngine::backfill_from_snapshot retired (2SM4Y7)"
    )
    # The non-DB path uses the snapshot_seed_block setter.
    assert hasattr(engine, "snapshot_seed_block")  # getter+setter (2SM4Y7)


def test_start_stashes_snapshot_and_backfill_blocks_for_two_step_verify(monkeypatch) -> None:
    """T1 (ADR-006 D4 + two-step verify prep): start() stashes the snapshot
    block (min newest_block) and the backfill target (from subscribe) on the
    registry, so the per-pool two-step verify (T6) can pass them to the verify
    closures without re-deriving. These are NOT wired into engine.set_verify_*
    (that dead path is deleted in T5) — they live on the registry for T6 to read.
    No behavior change yet beyond setting the fields (T6 reads them)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    v3_snap = _FakeSnapshot(newest_block=18_000_100)
    v4_snap = _FakeSnapshot(newest_block=18_000_050)

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=v3_snap,
        v4_snapshot=v4_snap,
    )

    # snapshot_block is the min of the supplied newest_blocks. The registry
    # no longer stashes a backfill_block for step-2 — the post-drain pin carries
    # its OWN block (captured atomically with the drain), so step-2 takes no
    # `block` argument from the registry (2026-06-29 fix).
    assert registry._verify_snapshot_block == 18_000_050
    assert not hasattr(registry, "_verify_backfill_block")


def test_start_stashes_None_blocks_when_no_snapshots() -> None:
    """With no snapshots, there's no snapshot block to derive and no backfill
    applied, so the snapshot block stash is None (T6 will guard on None ==
    verify not applicable for this pool). The registry no longer stashes a
    backfill_block — step-2 pins its own block."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    assert registry._verify_snapshot_block is None
    assert not hasattr(registry, "_verify_backfill_block")
