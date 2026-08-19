"""DMZ3DD: EngineRegistry register_vN_pool TOCTOU under concurrent workers.

`register_v3_pool` / `register_v4_pool` short-circuit on
`pool.address in self._vN_keys` but set the cache only AFTER the
`await engine.run_vN_registration_lifecycle(...)` (a blocking-RPC verify).
On the single asyncio loop that await is a yield point: under REG_WORKERS
concurrent registration workers sharing a pool, N workers can all pass the
check before any sets the cache, so the same pool's verify lifecycle runs N
times (real re-verification, wasted RPC + a false-mismatch risk on the tight
post-drain verify).

The fix claims an in-flight entry BEFORE the await; a worker that sees the
claim awaits the SAME shared future, so a pool is verified at most once.
These tests drive N concurrent workers at the same pool and assert the verify
lifecycle runs exactly once (RED before the claim, GREEN after).

Uses the `engine=` testability seam (no real ArbitrageEngine / RPC / BotState)
with a counting fake lifecycle that awaits to widen the race window.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.utils.bytes import to_0x_hex, to_bytes

V3_ADDR = "0x" + "1" * 40
V4_MANAGER = "0x" + "2" * 40
V3_POOL_ID = 101
V4_POOL_ID = 42
N_WORKERS = 8


class _CountingFakeEngine:
    """Fake engine whose verify lifecycles count invocations and await (to
    widen the race window so all `N_WORKERS` workers pass the cache check
    before the first completes)."""

    def __init__(self) -> None:
        self.v3_lifecycle_calls = 0
        self.v4_lifecycle_calls = 0

    async def run_v3_registration_lifecycle(self, address: str, snapshot_block: object) -> None:
        self.v3_lifecycle_calls += 1
        await asyncio.sleep(0.005)

    async def run_v4_registration_lifecycle(
        self, address: str, pool_id_hex: str, snapshot_block: object
    ) -> None:
        self.v4_lifecycle_calls += 1
        await asyncio.sleep(0.005)


def _fake_v3_pool(pool_id: int) -> SimpleNamespace:
    return SimpleNamespace(address=V3_ADDR, _py_pool=SimpleNamespace(pool_id=pool_id))


def _fake_v4_pool(pool_id: int) -> SimpleNamespace:
    return SimpleNamespace(
        address=V4_MANAGER,
        pool_id=to_bytes(b"\x01" * 32),
        _py_pool=SimpleNamespace(pool_id=pool_id),
    )


async def test_v3_verify_lifecycle_runs_exactly_once_under_concurrent_workers() -> None:
    engine = _CountingFakeEngine()
    registry = EngineRegistry(engine=engine)  # type: ignore[arg-type]
    pool = _fake_v3_pool(V3_POOL_ID)

    results = await asyncio.gather(*(registry.register_v3_pool(pool) for _ in range(N_WORKERS)))

    assert engine.v3_lifecycle_calls == 1, (
        f"verify lifecycle ran {engine.v3_lifecycle_calls}x, expected 1 (TOCTOU)"
    )
    assert set(results) == {V3_POOL_ID}
    assert len(registry._v3_keys) == 1
    assert registry._v3_keys[V3_ADDR] == V3_POOL_ID


async def test_v4_verify_lifecycle_runs_exactly_once_under_concurrent_workers() -> None:
    engine = _CountingFakeEngine()
    registry = EngineRegistry(engine=engine)  # type: ignore[arg-type]
    pool = _fake_v4_pool(V4_POOL_ID)
    pid_hex = to_0x_hex(pool.pool_id)

    results = await asyncio.gather(*(registry.register_v4_pool(pool) for _ in range(N_WORKERS)))

    assert engine.v4_lifecycle_calls == 1, (
        f"verify lifecycle ran {engine.v4_lifecycle_calls}x, expected 1 (TOCTOU)"
    )
    assert set(results) == {V4_POOL_ID}
    assert len(registry._v4_keys) == 1
    assert registry._v4_keys[pid_hex] == V4_POOL_ID
