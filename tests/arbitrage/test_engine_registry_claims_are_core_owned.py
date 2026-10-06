"""The at-most-once verify claim and pool identity are CORE-owned.

`register_v3_pool` / `register_v4_pool` used to short-circuit on a Python
`_vN_keys` cache and close a check-then-act window with a Python
`VerifyClaims` table keyed on the same key. Both are gone: the pool's
`pool_id` is derived from the shared `BotState`, and the lifecycle call itself
enters the driver's session owner (`PoolVerifications` in
`degenbot-bot::bot_core::verify_claims`), so concurrent registration workers
share one lifecycle run and a completed lifecycle is a durable core fact that
makes a later registration a no-op.

The policy matrix (leader once / peers share the outcome / release-on-failure /
identity-checked release) is pinned mechanically in Rust at
`bot_core::verify_claims::tests`, where the lifecycle and the claim live
together. What is pinned HERE is the Python seam: the registry keeps no
identity or claim state, and every registration dispatches exactly one
core-owned lifecycle call, so the invariant cannot be re-implemented or
bypassed on this side of the boundary.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.utils.bytes import to_0x_hex, to_bytes

V3_ADDR = "0x" + "1" * 40
V4_MANAGER = "0x" + "2" * 40
V3_POOL_ID = 101
V4_POOL_ID = 42
N_WORKERS = 8


class _CountingFakeEngine:
    """Fake engine that counts lifecycle dispatches and awaits.

    It stands in for the CORE side of the boundary: the count is how many times
    the claim table was *entered*. A Python-side claim table would have kept
    the second and later workers off this call entirely — that is exactly the
    state that no longer exists, and the reason this count is N and not 1.
    """

    def __init__(self, lifecycle_gate: asyncio.Barrier | None = None) -> None:
        self.v3_lifecycle_calls = 0
        self.v4_lifecycle_calls = 0
        # When set, every lifecycle entry waits for all workers to arrive
        # before returning: the overlap is arranged, not scheduled.
        self._lifecycle_gate = lifecycle_gate

    async def run_v3_registration_lifecycle(self, address: str, snapshot_block: object) -> None:
        self.v3_lifecycle_calls += 1
        await self._hold_for_overlap()

    async def run_v4_registration_lifecycle(
        self, address: str, pool_id_hex: str, snapshot_block: object
    ) -> None:
        self.v4_lifecycle_calls += 1
        await self._hold_for_overlap()

    async def _hold_for_overlap(self) -> None:
        if self._lifecycle_gate is not None:
            await self._lifecycle_gate.wait()


@dataclass
class _PoolHandle:
    """The Rust pool-handle slice the registry reads: the engine pool id."""

    pool_id: int


@dataclass
class _FakeV3Pool:
    """Pool double carrying the identity slice ``register_v3_pool`` reads."""

    address: str
    _py_pool: _PoolHandle


@dataclass
class _FakeV4Pool:
    """Pool double carrying the identity slice ``register_v4_pool`` reads."""

    address: str
    pool_id: bytes
    _py_pool: _PoolHandle


def _fake_v3_pool(pool_id: int) -> _FakeV3Pool:
    return _FakeV3Pool(address=V3_ADDR, _py_pool=_PoolHandle(pool_id=pool_id))


def _fake_v4_pool(pool_id: int) -> _FakeV4Pool:
    return _FakeV4Pool(
        address=V4_MANAGER,
        pool_id=to_bytes(b"\x01" * 32),
        _py_pool=_PoolHandle(pool_id=pool_id),
    )


def test_registry_keeps_no_pool_id_mirror_and_no_claim_table() -> None:
    """The Python registry holds no identity map, claim, or verified-pool set.

    All were mirrors of core state: `_v2/_v3/_v4_keys` duplicated the shared
    `BotState` registration tables, and `_v3/_v4_inflight` + their `VerifyClaims`
    duplicated the driver's session claim, while a `_verified` set would
    duplicate the driver's durable verify-once fact. Either could disagree with
    the core; none may exist.
    """
    registry = EngineRegistry(engine=_CountingFakeEngine())  # type: ignore[arg-type]

    forbidden = [
        name
        for name in vars(registry)
        if name.endswith(("_keys", "_inflight", "_claims", "_verified"))
    ]
    assert forbidden == [], f"EngineRegistry regrew core-owned state: {forbidden}"


async def test_concurrent_v3_registrations_dispatch_one_core_lifecycle_each() -> None:
    """N concurrent workers each enter the core claim exactly once.

    The registry adds no dedup of its own: the at-most-once guarantee for the
    pool's *lifecycle run* is the claim table inside the driver, entered once
    per registration. A Python-side table would have collapsed these N entries
    into one call here, which is precisely the state this migration removed.
    """
    engine = _CountingFakeEngine(lifecycle_gate=asyncio.Barrier(N_WORKERS))
    registry = EngineRegistry(engine=engine)  # type: ignore[arg-type]
    pool = _fake_v3_pool(V3_POOL_ID)

    # Deadlock guard only — the gate arranges the overlap; this converts a
    # worker that never arrives into a failure instead of a hang.
    results = await asyncio.wait_for(
        asyncio.gather(*(registry.register_v3_pool(pool) for _ in range(N_WORKERS))),
        timeout=10.0,
    )

    assert engine.v3_lifecycle_calls == N_WORKERS, (
        "each registration enters the core claim; the core decides at-most-once"
    )
    assert set(results) == {V3_POOL_ID}, (
        "every worker reads the same core-owned pool id"
    )


async def test_concurrent_v4_registrations_dispatch_one_core_lifecycle_each() -> None:
    """The V4 twin, keyed by the (PoolManager, pool_id) pair in the core."""
    engine = _CountingFakeEngine(lifecycle_gate=asyncio.Barrier(N_WORKERS))
    registry = EngineRegistry(engine=engine)  # type: ignore[arg-type]
    pool = _fake_v4_pool(V4_POOL_ID)

    # Deadlock guard only — the gate arranges the overlap; this converts a
    # worker that never arrives into a failure instead of a hang.
    results = await asyncio.wait_for(
        asyncio.gather(*(registry.register_v4_pool(pool) for _ in range(N_WORKERS))),
        timeout=10.0,
    )

    assert engine.v4_lifecycle_calls == N_WORKERS
    assert set(results) == {V4_POOL_ID}


async def test_v4_registration_passes_the_manager_and_pool_id_pair() -> None:
    """The claim key the core builds needs the pair, so the adapter passes both."""
    seen: list[tuple[str, str]] = []

    class _Recorder(_CountingFakeEngine):
        async def run_v4_registration_lifecycle(
            self, address: str, pool_id_hex: str, snapshot_block: object
        ) -> None:
            seen.append((address, pool_id_hex))
            await super().run_v4_registration_lifecycle(address, pool_id_hex, snapshot_block)

    registry = EngineRegistry(engine=_Recorder())  # type: ignore[arg-type]
    pool = _fake_v4_pool(V4_POOL_ID)

    await registry.register_v4_pool(pool)

    assert seen == [(V4_MANAGER, to_0x_hex(pool.pool_id))]
