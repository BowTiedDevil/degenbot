"""T6 (ADR-006 D4): fail-fast two-step verify at the registry drain seam.

`engine_registry.register_v3_pool` / `register_v4_pool` run the two-step
verify (snapshot block + backfill block) around their `apply_buffer_*` drain,
gated by verify config. A mismatch raises at the offending pool — surfacing
from `build_paths` instead of 18k pools later at the end-of-discovery batch.

V2 has no tick map — skip verify for V2.
"""

from __future__ import annotations

import asyncio
import inspect

import pytest

import degenbot.arbitrage.engine_registry as runner
from degenbot.degenbot_rs import VerificationMismatchError


class _RecordingVerifyEngine:
    """Fake engine that records verify calls and can be made to fail a phase.

    Mirrors the PyBot verify seam (async, via future_into_py) — the registry
    `await`s `verify_liquidity_maps` (or `verify_v3/v4_liquidity_maps`). A
    plain async function here stands in for the pyo3 coroutine.
    """

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.verify_calls: list[dict] = []
        self.fail_next: str | None = None
        self._last_processed_block: int | None = 18_000_042
        # JUCFCB: the DB path reads `snapshot_seed_block` from the engine.
        self._snapshot_seed_block: int | None = None

    @property
    def snapshot_seed_block(self) -> int | None:
        return self._snapshot_seed_block

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, value: int | None) -> None:
        self.calls.append("set_snapshot_seed_block")
        self._snapshot_seed_block = value

    # lifecycle (subscribe/backfill) — minimal, record-only
    def subscribe(self, ws: str) -> int:
        self.calls.append("subscribe")
        return 18_000_000

    def backfill_from_snapshot(self, rpc: str, snapshot_block: int) -> int:
        self.calls.append("backfill")
        return 0

    def set_verify_rpc_url(self, rpc: str) -> None:
        self.calls.append("set_verify_rpc_url")

    def set_verify_state_view(self, addr: str) -> None:
        self.calls.append("set_verify_state_view")

    def last_processed_block(self) -> int | None:
        return self._last_processed_block

    # the drain (already in place) — apply_buffer_*
    def apply_buffer_v3(self, address: str) -> None:
        self.calls.append(f"apply_buffer_v3:{address}")

    def apply_buffer_v4(self, address: str, pool_id_hex: str) -> None:
        self.calls.append(f"apply_buffer_v4:{pool_id_hex}")

    # the verify seam (async). CBCH6H: the two-step verify routes step-1
    # (snapshot) through the *_snapshot_seed variants (pinned seed, not
    # engine-current) and step-2 (backfill) through the *_post_drain_snapshot
    # variants (pinned post-drain tick_data, not engine-current — the step-2
    # rolling-start race fix). Both recorded with a `seed` flag so assertions
    # can pin which step fired.
    async def verify_v3_snapshot_seed(
        self, address: str, rpc_url: str, block_number: int | None
    ) -> None:
        self.verify_calls.append({
            "phase": "v3",
            "block": block_number,
            "rpc": rpc_url,
            "seed": True,
            "method": "snapshot_seed",
        })
        if self.fail_next == "v3":
            raise VerificationMismatchError("synthetic V3 seed tick mismatch")

    async def verify_v3_post_drain_snapshot(
        self, address: str, rpc_url: str
    ) -> None:
        self.verify_calls.append({
            "phase": "v3",
            "block": None,
            "rpc": rpc_url,
            "seed": False,
            "method": "post_drain",
        })

    async def verify_v3_liquidity_maps(self, rpc_url: str, block_number: int | None) -> None:
        # Batch verify (post-build_paths) — not exercised by the two-step
        # tests, but kept on the fake for API parity.
        self.verify_calls.append({
            "phase": "v3-batch",
            "block": block_number,
            "rpc": rpc_url,
            "seed": False,
        })

    async def verify_v4_snapshot_seed(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
        block_number: int | None,
    ) -> None:
        self.verify_calls.append({
            "phase": "v4",
            "block": block_number,
            "rpc": rpc_url,
            "sv": state_view_address,
            "seed": True,
            "method": "snapshot_seed",
        })
        if self.fail_next == "v4":
            raise VerificationMismatchError("synthetic V4 seed tick mismatch")

    async def verify_v4_post_drain_snapshot(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        rpc_url: str,
        state_view_address: str,
    ) -> None:
        self.verify_calls.append({
            "phase": "v4",
            "block": None,
            "rpc": rpc_url,
            "sv": state_view_address,
            "seed": False,
            "method": "post_drain",
        })

    async def verify_v4_liquidity_maps(
        self, rpc_url: str, state_view_address: str, block_number: int | None
    ) -> None:
        self.verify_calls.append({
            "phase": "v4-batch",
            "block": block_number,
            "rpc": rpc_url,
            "sv": state_view_address,
            "seed": False,
        })


class _FakeSnapshot:
    def __init__(self, newest_block: int) -> None:
        self.newest_block = newest_block


class _FakeV3Pool:
    """Minimal V3 pool double exposing the attributes register_v3_pool reads."""

    address = "0xV3POOL"

    class _PyPool:
        pool_id = 7

    _py_pool = _PyPool()


class _FakeV4Pool:
    """Minimal V4 pool double exposing the attributes register_v4_pool reads."""

    class _PoolId:
        def to_0x_hex(self) -> str:
            return "0xV4POOLID"

    pool_id = _PoolId()
    address = "0xV4PM"

    class _PyPool:
        pool_id = 9

    _py_pool = _PyPool()


def _registry_started_with_snapshots(
    monkeypatch,
) -> tuple[runner.EngineRegistry, _RecordingVerifyEngine]:
    fake = _RecordingVerifyEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    # Stub the stream fns (same as the start tests) — they need a full DB-backed
    # snapshot; here we only exercise start()'s orchestration + drain/verify.
    monkeypatch.setattr(runner, "stream_v3_snapshot_to_engine", lambda *a, **k: None)
    monkeypatch.setattr(runner, "stream_v4_snapshot_to_engine", lambda *a, **k: None)
    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=_FakeSnapshot(18_000_100),
        v4_snapshot=_FakeSnapshot(18_000_050),
        verify_state_view="0x0000000000000000000000000000000000000abc",
    )
    # Confirm T1: the snapshot seed block was stashed (step-1 compares the
    # pinned seed vs on-chain@this block). step-2 captures its OWN block via
    # the post-drain pin, so the registry no longer stashes a backfill-block
    # constant.
    assert registry._verify_snapshot_block == 18_000_050
    assert not hasattr(registry, "_verify_backfill_block"), (
        "_verify_backfill_block is dead — step-2 pins its own block"
    )
    return registry, fake


def test_register_v3_pool_runs_two_step_verify_around_drain(monkeypatch) -> None:
    """T6: register_v3_pool runs snapshot-verify BEFORE the apply_buffer_v3
    drain and backfill-verify AFTER — the two-step fail-fast at the seam the
    dead engine.register_v3 gate was meant to provide. Verification is async
    (awaited, not block_on). V2 has no tick map; it skips verify."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    assert inspect.iscoroutinefunction(registry.register_v3_pool)

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    key = asyncio.run(_go())

    assert key == 7
    # TWO verify calls: snapshot block (18_000_050) BEFORE the drain, then
    # step-2 (post-drain) AFTER — the two-step fail-fast. Step-2's block is
    # NOT supplied by the registry — the engine pins the block atomically with
    # the drain (the `update_block` at pin time), so `block=None` here (the
    # registry no longer passes `verify_backfill_block` as a constant — that
    # fabricated a mismatch on active pools during a slow build_paths).
    blocks = [c["block"] for c in fake.verify_calls]
    assert blocks == [18_000_050, None], f"snapshot-then-pinned-block, got {fake.verify_calls}"
    # CBCH6H: step-1 (snapshot) routes through the seed-verify (pinned seed,
    # not engine-current); step-2 (backfill) through the current-state verify.
    seed_flags = [c["seed"] for c in fake.verify_calls]
    assert seed_flags == [True, False], (
        f"step-1 must verify the seed, step-2 the current; got {fake.verify_calls}"
    )
    methods = [c["method"] for c in fake.verify_calls]
    assert methods == ["snapshot_seed", "post_drain"], (
        f"step-1 must route through *_snapshot_seed, step-2 through *_post_drain_snapshot (the rolling-start race fix); got {fake.verify_calls}"
    )
    drain_idx = next(i for i, c in enumerate(fake.calls) if "apply_buffer_v3" in c)
    # the drain ran exactly once, between the two verify awaits (verify is the
    # only thing appending to verify_calls; the drain is the only thing
    # appending "apply_buffer_v3" to calls — order between the two lists isn't
    # observable here, but exactly-one drain + two verifies is the contract).
    assert sum("apply_buffer_v3" in c for c in fake.calls) == 1


def test_register_v4_pool_runs_two_step_verify_around_drain(monkeypatch) -> None:
    """T6: register_v4_pool runs the two-step verify around apply_buffer_v4."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    assert inspect.iscoroutinefunction(registry.register_v4_pool)

    async def _go() -> int:
        return await registry.register_v4_pool(_FakeV4Pool())

    key = asyncio.run(_go())

    assert key == 9
    blocks = [c["block"] for c in fake.verify_calls]
    # Step-1 (snapshot_seed) carries the registry-supplied snapshot block;
    # step-2 (post_drain) pins its own block in the engine (None here).
    assert blocks == [18_000_050, None], f"snapshot-then-pinned-block, got {fake.verify_calls}"
    seed_flags = [c["seed"] for c in fake.verify_calls]
    assert seed_flags == [True, False], (
        f"step-1 must verify the seed, step-2 the current; got {fake.verify_calls}"
    )
    methods = [c["method"] for c in fake.verify_calls]
    assert methods == ["snapshot_seed", "post_drain"], (
        f"step-1 → *_snapshot_seed, step-2 → *_post_drain_snapshot; got {fake.verify_calls}"
    )
    assert sum("apply_buffer_v4" in c for c in fake.calls) == 1


def test_register_v3_pool_fail_fast_raises_at_offending_pool(monkeypatch) -> None:
    """T6: a snapshot seed mismatch raises VerificationMismatchError at the
    offending pool, surfacing from build_paths — not 18k pools later."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    fake.fail_next = "v3"

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    with pytest.raises(VerificationMismatchError, match="synthetic V3 seed"):
        asyncio.run(_go())


def test_register_skips_verify_when_config_not_stashed() -> None:
    """When start() wasn't called with verify config (no state_view), the
    per-pool verify is skipped — NOT a VerificationMismatchError. Mirrors
    the batch verify_liquidity_maps config gate."""
    fake = _RecordingVerifyEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    # start() WITHOUT verify_state_view
    registry.start("http://localhost:8545", "ws://localhost:8546")

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    key = asyncio.run(_go())
    assert key == 7
    assert fake.verify_calls == [], "verify must be skipped without config"


def test_register_v3_pool_idempotent_skips_verify(monkeypatch) -> None:
    """A pool already in the cache short-circuits before the drain+verify —
    no double-verify on the second path that touches the same pool."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    k1 = asyncio.run(_go())
    k2 = asyncio.run(_go())
    assert k1 == k2 == 7
    # first register: 2 verifies (snapshot + backfill); second (cached): 0.
    assert len(fake.verify_calls) == 2, "second register must short-circuit verify"
