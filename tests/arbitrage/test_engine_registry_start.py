"""Integration tests for EngineRegistry.start — the two-phase Layer A facade.

`start(...)` runs the pre-pump startup ritual (subscribe → stream snapshots →
backfill → verify config) and stops at `EnginePhase::Backfilled`, BEFORE
`resume()`. This is the consumer-safety invariant: between subscribe and
resume (including stream/backfill/verify) zero result batches are emitted,
so the caller can attach its consumer any time before `resume()` without
risk of unbounded backlog or stale-batch dispatch.
"""

from __future__ import annotations

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

    def set_verify_on_register(self, enabled: bool) -> None:
        self.calls.append("set_verify_on_register")

    def verify_liquidity_maps(
        self,
        *,
        rpc_url: str,
        tick_lens_address: str,
        state_view_address: str,
        block_number: int | None,
    ) -> None:
        self.calls.append("verify_liquidity_maps")
        self.verify_args = {
            "rpc_url": rpc_url,
            "tick_lens_address": tick_lens_address,
            "state_view_address": state_view_address,
            "block_number": block_number,
        }

    def resume(self) -> None:
        self.calls.append("resume")


def test_start_no_snapshots_calls_subscribe_then_verify_never_resume() -> None:
    """Tracer: with no snapshots, start() subscribes + sets verify config in
    order and never calls resume(). Skip stream/backfill (no snapshots)."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    backfill_target = registry.start(
        "http://node:8545",
        "ws://node:8546",
    )

    assert fake.calls == [
        "subscribe",
        "set_verify_rpc_url",
        "set_verify_on_register",
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
        "http://node:8545",
        "ws://node:8546",
        v3_snapshot=v3_snap,
        v4_snapshot=v4_snap,
    )

    # The derived block is the min of the two newest_blocks.
    assert fake.backfill_args == [("http://node:8545", 18_000_050)]
    # Streams ran for both snapshots, in order, then backfill, then verify.
    assert fake.calls == [
        "subscribe",
        "stream_v3",
        "stream_v4",
        "backfill",
        "set_verify_rpc_url",
        "set_verify_on_register",
    ]
    assert "resume" not in fake.calls


def test_start_passes_verify_state_view_when_supplied() -> None:
    """verify_state_view is set on the engine only when supplied."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    registry.start(
        "http://node:8545",
        "ws://node:8546",
        verify_state_view=state_view,
    )

    assert "set_verify_state_view" in fake.calls


def test_start_skips_set_verify_state_view_when_none() -> None:
    """When verify_state_view is None, set_verify_state_view is not called."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://node:8545", "ws://node:8546")

    assert "set_verify_state_view" not in fake.calls


def test_verify_liquidity_maps_raises_when_start_not_called() -> None:
    """Before start() stashes verify config, verify_liquidity_maps is a
    RuntimeError — never a silent skip or an unconfigured-RPC failure deep in
    the engine. Mirrors the WFDTUR fail-fast posture."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)

    with pytest.raises(RuntimeError, match="verify config"):
        registry.verify_liquidity_maps()
    assert "verify_liquidity_maps" not in fake.calls


def test_verify_liquidity_maps_delegates_with_stashed_config() -> None:
    """After start(..., verify_state_view=...), verify_liquidity_maps delegates
    to the engine with the stashed RPC + StateView, a zero tick_lens (unused by
    the V3 batch path), and block_number=None (latest). Emits exactly one
    delegate call — the [verify] line the analyzer keys on."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    registry.start(
        "http://node:8545",
        "ws://node:8546",
        verify_state_view=state_view,
    )

    registry.verify_liquidity_maps()

    assert fake.verify_args == {
        "rpc_url": "http://node:8545",
        "tick_lens_address": "0x0000000000000000000000000000000000000000",
        "state_view_address": state_view,
        "block_number": None,
    }


def test_verify_liquidity_maps_raises_when_state_view_omitted() -> None:
    """V4 batch verify needs StateView; start() without verify_state_view must
    surface as a config error at verify time, not a silent pass."""
    fake = FakeEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start("http://node:8545", "ws://node:8546")  # no state_view

    with pytest.raises(RuntimeError, match="verify config"):
        registry.verify_liquidity_maps()
