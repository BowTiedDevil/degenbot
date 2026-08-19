"""IKGQ6F / ADR-022: registry delegation + D1 core-owned verify-lifecycle.

`engine_registry.register_v3_pool` / `register_v4_pool` are now THIN
delegating shells: each makes ONE call to the core-owned
`run_v3_registration_lifecycle(pool_addr, snapshot_block)` /
`run_v4_registration_lifecycle(pm, pool_id, snapshot_block)`, which sequences
the D4 lifecycle in Rust — quarantine (6N7XVR) → seed-verify @ snapshot block
(CBCH6H) → drain+pin (single core.write() hold) → post-drain-verify @ the
pin's own block → set_live, with the mismatch tripwire as the final gate.

The step ordering / quarantine-before-verify / live-after-drain semantics are
now asserted in the Rust unit tests
(`bot_core::registration_lifecycle::tests`); these Python tests pin the thin
delegation seam + the D-C no-config posture at the registry boundary.
"""

from __future__ import annotations

import asyncio
import inspect

import pytest

import degenbot.arbitrage.engine_registry as runner
from degenbot.exceptions import VerificationMismatchError


class _RecordingVerifyEngine:
    """Fake engine that records the core-owned lifecycle delegation call.

    Mirrors the `PyArbitrageEngine.run_v3/v4_registration_lifecycle` pyo3
    seam (async, via future_into_py — the registry `await`s it standing in for
    a coroutine). The step ordering inside the core lifecycle is NOT observable
    here; those invariants are asserted by the Rust `registration_lifecycle`
    unit tests.
    """

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.run_calls: list[dict] = []
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

    def load_v3_snapshot_from_py(self, py_data: object) -> None:
        self.calls.append("load_v3_snapshot_from_py")

    def load_v4_snapshot_from_py(self, py_data: object) -> None:
        self.calls.append("load_v4_snapshot_from_py")

    def set_verify_rpc_url(self, rpc: str) -> None:
        self.calls.append("set_verify_rpc_url")

    def set_verify_state_view(self, addr: str) -> None:
        self.calls.append("set_verify_state_view")

    def last_processed_block(self) -> int | None:
        return self._last_processed_block

    # IKGQ6F / ADR-022 D1: the single core-owned lifecycle entry point. A
    # mismatch (fail_next set) surfaces as VerificationMismatchError — the
    # tripwire propagates from build_paths without auto-repair.
    async def run_v3_registration_lifecycle(
        self, address: str, snapshot_block: int | None
    ) -> None:
        self.run_calls.append({
            "family": "v3",
            "address": address,
            "snapshot_block": snapshot_block,
        })
        if self.fail_next == "v3":
            raise VerificationMismatchError("synthetic V3 seed tick mismatch")

    async def run_v4_registration_lifecycle(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        snapshot_block: int | None,
    ) -> None:
        self.run_calls.append({
            "family": "v4",
            "address": pool_manager_address,
            "pool_id": pool_id_hex,
            "snapshot_block": snapshot_block,
        })
        if self.fail_next == "v4":
            raise VerificationMismatchError("synthetic V4 seed tick mismatch")


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
    # XEANMB: `load_*_from_py` is retired; `start()` sets `snapshot_seed_block`
    # from `min(newest_block)` directly. `S = min(newest_block)` across the
    # supplied snapshots is stashed as `_verify_snapshot_block` and passed to
    # the core lifecycle (step-1 seed verify @ snapshot block).
    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        v3_snapshot=_FakeSnapshot(18_000_100),
        v4_snapshot=_FakeSnapshot(18_000_050),
        verify_state_view="0x0000000000000000000000000000000000000abc",
    )
    assert registry._verify_snapshot_block == 18_000_050
    return registry, fake


def test_register_v3_pool_delegates_to_core_lifecycle(monkeypatch) -> None:
    """D1: register_v3_pool makes ONE core-owned call, passing the stashed
    snapshot block (step-1 seed-verify target). The drain/pin/step-2/live
    ordering is owned inside the core lifecycle (Rust-tested)."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    assert inspect.iscoroutinefunction(registry.register_v3_pool)

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    key = asyncio.run(_go())

    assert key == 7
    assert len(fake.run_calls) == 1, "exactly one core lifecycle call per register"
    assert fake.run_calls[0] == {
        "family": "v3",
        "address": "0xV3POOL",
        "snapshot_block": 18_000_050,
    }


def test_register_v4_pool_delegates_to_core_lifecycle(monkeypatch) -> None:
    """D1: register_v4_pool makes ONE core-owned call (pm + pool_id + block)."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    assert inspect.iscoroutinefunction(registry.register_v4_pool)

    async def _go() -> int:
        return await registry.register_v4_pool(_FakeV4Pool())

    key = asyncio.run(_go())

    assert key == 9
    assert len(fake.run_calls) == 1
    assert fake.run_calls[0] == {
        "family": "v4",
        "address": "0xV4PM",
        "pool_id": "0xV4POOLID",
        "snapshot_block": 18_000_050,
    }


@pytest.mark.parametrize("family", ["v3", "v4"])
def test_register_fail_fast_surfaces_error_to_racing_sibling(
    monkeypatch,
    family: str,
) -> None:
    """A sibling that claims the DMZ3DD inflight entry while a tripwired
    lifecycle is in flight must receive the VerificationMismatchError DIRECTLY
    from the shared claim (not a hang, cancel, or dropped future)."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    inflight = (registry._v3_inflight, "0xV3POOL") if family == "v3" else (
        registry._v4_inflight, "0xV4POOLID",
    )
    inflight, key = inflight
    register = getattr(registry, f"register_{family}_pool")
    pool = _FakeV3Pool() if family == "v3" else _FakeV4Pool()
    seen: dict[str, object] = {}

    async def _sibling() -> None:
        try:
            await inflight[key]
        except BaseException as exc:  # noqa: BLE001 - record whatever surfaces
            seen["exc"] = exc

    async def _failing_lifecycle(*_args, **_kwargs):
        # Start the sibling, then yield once so it grabs the live claim BEFORE
        # the mismatch trips (ready-queue FIFO makes this deterministic).
        seen["sibling"] = asyncio.get_running_loop().create_task(_sibling())
        await asyncio.sleep(0)
        raise VerificationMismatchError(f"synthetic {family} seed tick mismatch")

    setattr(fake, f"run_{family}_registration_lifecycle", _failing_lifecycle)

    async def _go() -> None:
        with pytest.raises(
            VerificationMismatchError,
            match=f"synthetic {family} seed",
        ):
            await register(pool)
        await seen["sibling"]
        assert isinstance(seen["exc"], VerificationMismatchError), (
            f"racing sibling must receive the mismatch error directly, "
            f"got: {seen.get('exc')!r}"
        )

    asyncio.run(_go())


def test_register_v3_pool_fail_fast_surfaces_mismatch(monkeypatch) -> None:
    """The verification tripwire (D-A) propagates as VerificationMismatchError
    from the core lifecycle, surfacing from build_paths — not 18k pools later.
    No auto-repair: the exception is not caught by the registry."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)
    fake.fail_next = "v3"

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    with pytest.raises(VerificationMismatchError, match="synthetic V3 seed"):
        asyncio.run(_go())


# D-C (no verify-disabled mode for tracked): the registry no longer GATES the
# lifecycle on verify config being stashed. With D-B the verify provider is
# always the bot's single provider; the "no config" fail-fast (a missing V4
# `state_view` → typed error) is enforced INSIDE the core lifecycle, not by a
# Python-side skip. So register_* ALWAYS delegates — the old
# `test_register_skips_verify_when_config_not_stashed` contract is retired.
def test_register_always_delegates_even_without_verify_config() -> None:
    """D-C: without `verify_state_view` stashed, register_* STILL calls the
    core lifecycle — it never silently skips verification. Whether the pool
    can actually be released is decided in core (fail-fast if unreachable)."""
    fake = _RecordingVerifyEngine()
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start("http://localhost:8545", "ws://localhost:8546")

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    key = asyncio.run(_go())
    assert key == 7
    assert len(fake.run_calls) == 1, (
        "register must always delegate to the core lifecycle — a Python-side "
        "skip would reintroduce a verify-disabled path for tracked pools"
    )
    # no snapshots supplied → snapshot_block is None (step-1 no-op; step-2 +
    # tripwire still run core-side).
    assert fake.run_calls[0]["snapshot_block"] is None


def test_register_v3_pool_idempotent_skips_lifecycle(monkeypatch) -> None:
    """A pool already in the cache short-circuits before the core lifecycle —
    no second run on the next path that touches the same pool."""
    registry, fake = _registry_started_with_snapshots(monkeypatch)

    async def _go() -> int:
        return await registry.register_v3_pool(_FakeV3Pool())

    k1 = asyncio.run(_go())
    k2 = asyncio.run(_go())
    assert k1 == k2 == 7
    assert len(fake.run_calls) == 1, "second register must short-circuit the lifecycle"
