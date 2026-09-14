"""Discovery delivery batching (4IOEVT): red/green behavioral tests.

find_paths_async must deliver paths in batches (ONE event-loop hop per
batch) while preserving the sync find_paths stream byte-for-byte. The
worker-thread + bounded std-queue delivery shape is exercised here:

- parity vs the sync stream (batch_size 1 / default / larger-than-count),
- batch_size <= 1 legacy per-path cadence and the default batch cadence,
- cancellation (aclose) stops the worker (no zombie threads across
  repeated sweeps in one process),
- producer exceptions re-raise at the consumer,
- the typed pathfinding.discovery_batch_size config key plumbs through
  PathRegistrationPipeline.discovery_sweep and changes the observable
  batching cadence.
"""

from __future__ import annotations

import asyncio
import os
import subprocess
import sys
import threading
import time
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest

from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models import Erc20TokenTable, UniswapV2PoolTable
from degenbot.database.models.base import ExchangeTable
from degenbot.database.operations import (
    create_new_sqlite_database,
    get_scoped_sqlite_session,
)
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.pathfinding import _pathfinding, find_paths, find_paths_async
from degenbot.runner.build_paths import PathRegistrationPipeline
from degenbot.types.chain import ChainId

if TYPE_CHECKING:
    import pathlib

CHAIN = ChainId.ETH
WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN_A_ADDR = "0x" + "AA" * 20
POOL_A_ADDR = "0x" + "11" * 20
POOL_B_ADDR = "0x" + "12" * 20

_BASE_KWARGS: dict[str, object] = {
    "chain_id": CHAIN,
    "start_tokens": [WETH_ADDR],
    "end_tokens": [WETH_ADDR],
    "max_depth": 2,
    "pool_types": [UniswapV2PoolTable],
}


def _seed_two_pool_db(db_path: pathlib.Path) -> DatabaseSessionManager:
    """Seed a file-backed temp SQLite with a WETH<->A two-pool cycle."""
    create_new_sqlite_database(db_path)
    scoped = get_scoped_sqlite_session(database_path=db_path)
    session = scoped()
    try:
        exchange = ExchangeTable(
            chain_id=CHAIN, name="test", active=True, factory=ZERO_ADDRESS
        )
        session.add(exchange)
        session.flush()
        weth = Erc20TokenTable(chain=CHAIN, address=WETH_ADDR, symbol="WETH")
        token_a = Erc20TokenTable(chain=CHAIN, address=TOKEN_A_ADDR, symbol="A")
        session.add_all([weth, token_a])
        session.flush()
        session.add(
            UniswapV2PoolTable(
                address=POOL_A_ADDR,
                chain=CHAIN,
                token0_id=weth.id,
                token1_id=token_a.id,
                exchange_id=exchange.id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
            )
        )
        session.add(
            UniswapV2PoolTable(
                address=POOL_B_ADDR,
                chain=CHAIN,
                token0_id=token_a.id,
                token1_id=weth.id,
                exchange_id=exchange.id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
            )
        )
        session.commit()
    finally:
        session.close()
    return DatabaseSessionManager(scoped)


@pytest.fixture
def db(tmp_path: pathlib.Path) -> DatabaseSessionManager:
    return _seed_two_pool_db(tmp_path / "discovery_batching.db")


def _path_signature(paths: list[object]) -> list[tuple[tuple[str, str | None, object], ...]]:
    return [
        tuple((step.address, step.hash, step.type) for step in path)  # type: ignore[attr-defined]
        for path in paths
    ]


async def _collect(**kwargs: object) -> list[object]:
    return [path async for path in find_paths_async(**kwargs)]  # type: ignore[arg-type]


def test_batched_async_matches_sync_stream(db: DatabaseSessionManager) -> None:
    """Content + order parity: 1, small, default, and oversized batches."""
    expected = list(find_paths(db=db, **_BASE_KWARGS))  # type: ignore[arg-type]
    assert expected, "fixture graph produced no paths"

    for batch_size in (1, 2, 1000, 10**6):
        got = asyncio.run(_collect(db=db, batch_size=batch_size, **_BASE_KWARGS))
        assert _path_signature(got) == _path_signature(expected), (
            f"batched stream diverged at batch_size={batch_size}"
        )


def _install_fake_producer(
    monkeypatch: pytest.MonkeyPatch,
    *,
    n: int | None = None,
    items: list[object] | None = None,
    boom_after: int | None = None,
    boom: type[BaseException] | None = None,
) -> None:
    """Patch the sync producer find_paths with a deterministic stream."""

    def fake(**kwargs: object) -> object:
        seq = items if items is not None else [f"p{i}" for i in range(n or 0)]
        for i, item in enumerate(seq):
            if boom_after is not None and i >= boom_after:
                raise boom("producer died")  # type: ignore[misc]
            yield [item]
        if boom_after is not None and boom_after >= len(seq):
            raise boom("producer died")  # type: ignore[misc]

    monkeypatch.setattr(_pathfinding, "find_paths", fake)


async def _drain_with_sleep_count(
    monkeypatch: pytest.MonkeyPatch,
    *,
    n: int,
    batch_size: int,
) -> tuple[list[object], list[float]]:
    """Drain the async stream, counting asyncio.sleep (one per batch hop)."""
    _install_fake_producer(monkeypatch, n=n)
    sleeps: list[float] = []
    real_sleep = asyncio.sleep

    async def counting_sleep(delay: float = 0) -> None:
        sleeps.append(delay)
        await real_sleep(0)

    monkeypatch.setattr(_pathfinding.asyncio, "sleep", counting_sleep)

    got = [
        path
        async for path in find_paths_async(
            chain_id=1, start_tokens=[], end_tokens=[], db=None, batch_size=batch_size
        )
    ]
    return got, sleeps


async def test_batch_size_one_is_legacy_per_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """batch_size=1 -> one event-loop hop per path (legacy cadence)."""
    got, sleeps = await _drain_with_sleep_count(monkeypatch, n=10, batch_size=1)
    assert got == [[f"p{i}"] for i in range(10)]
    assert len(sleeps) == 10


async def test_default_batch_size_batches_delivery(monkeypatch: pytest.MonkeyPatch) -> None:
    """A large batch -> ONE event-loop hop for the whole stream."""
    got, sleeps = await _drain_with_sleep_count(monkeypatch, n=10, batch_size=1000)
    assert got == [[f"p{i}"] for i in range(10)]
    assert len(sleeps) == 1


async def test_batch_size_bounds_hop_count(monkeypatch: pytest.MonkeyPatch) -> None:
    """batch_size=3 over 10 paths -> ceil(10/3) == 4 hops."""
    got, sleeps = await _drain_with_sleep_count(monkeypatch, n=10, batch_size=3)
    assert got == [[f"p{i}"] for i in range(10)]
    assert len(sleeps) == 4


def _wait_for(predicate: object, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():  # type: ignore[operator]
            return True
        time.sleep(0.05)
    return predicate()  # type: ignore[operator]


async def _partial_sweep(take: int, batch_size: int = 100) -> int:
    agen = find_paths_async(
        chain_id=1, start_tokens=[], end_tokens=[], db=None, batch_size=batch_size
    )
    seen = 0
    async for _path in agen:
        seen += 1
        if seen >= take:
            break
    await agen.aclose()
    return seen


async def test_aclose_stops_worker_no_zombies(monkeypatch: pytest.MonkeyPatch) -> None:
    """Repeated mid-sweep closes leave no worker threads behind."""
    _install_fake_producer(monkeypatch, n=1_000_000)

    # Warm the shared asyncio.to_thread executor so its cached threads are
    # part of the baseline; then let every transient thread settle.
    assert await _partial_sweep(5)
    time.sleep(0.3)
    baseline = threading.active_count()

    for _ in range(6):
        assert await _partial_sweep(5) == 5

    assert _wait_for(
        lambda: threading.active_count() <= baseline, timeout=5.0
    ), f"zombie worker threads: {threading.active_count()} > {baseline}"


class _BoomError(RuntimeError):
    pass


async def test_producer_exception_reraises_at_consumer(monkeypatch: pytest.MonkeyPatch) -> None:
    """A producer failure surfaces at the async consumer, not silently."""
    _install_fake_producer(monkeypatch, items=["a", "b"], boom_after=2, boom=_BoomError)

    got: list[object] = []
    with pytest.raises(_BoomError, match="producer died"):
        async for path in find_paths_async(
            chain_id=1, start_tokens=[], end_tokens=[], db=None, batch_size=2
        ):
            got.append(path)

    assert got == [["a"], ["b"]], "the completed batch must be delivered before the raise"


# ---------------------------------------------------------------------------
# Typed config plumbing (pathfinding.discovery_batch_size).
# ---------------------------------------------------------------------------


def _run_ffi_getter(env_overrides: dict[str, str]) -> str:
    env = dict(os.environ)
    env.update(env_overrides)
    out = subprocess.run(
        [
            sys.executable,
            "-c",
            "from degenbot._ffi import discovery_batch_size as f; print(f())",
        ],
        env=env,
        capture_output=True,
        text=True,
        check=True,
        timeout=300,
    )
    return out.stdout.strip().splitlines()[-1]


def test_typed_config_env_override_reaches_the_getter() -> None:
    assert _run_ffi_getter({"DEGENBOT_DISCOVERY_BATCH_SIZE": "7"}) == "7"


def test_typed_config_is_positive_clamped() -> None:
    assert _run_ffi_getter({"DEGENBOT_DISCOVERY_BATCH_SIZE": "0"}) == "1"


def _make_pipeline() -> PathRegistrationPipeline:
    ctx = SimpleNamespace(
        bot=SimpleNamespace(registration_fleet_hosted=lambda: True, _py_bot=None),
        chain_id=1,
        db=None,
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    return PathRegistrationPipeline(context=ctx, engine_registry=None)  # type: ignore[arg-type]


def test_discovery_sweep_passes_typed_batch_size(monkeypatch: pytest.MonkeyPatch) -> None:
    """discovery_sweep reads the typed key and forwards it to find_paths_async."""
    build_paths_module = sys.modules["degenbot.runner.build_paths"]

    monkeypatch.setattr(build_paths_module, "_discovery_batch_size", lambda: 42)
    captured: dict[str, object] = {}

    def fake_find_paths_async(**kwargs: object) -> None:
        captured.update(kwargs)

    monkeypatch.setattr(build_paths_module, "find_paths_async", fake_find_paths_async)

    _make_pipeline().discovery_sweep()
    assert captured["batch_size"] == 42


async def test_config_batch_size_changes_discovery_cadence(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The typed value changes observable batching granularity end-to-end."""
    build_paths_module = sys.modules["degenbot.runner.build_paths"]

    _install_fake_producer(monkeypatch, n=9)
    monkeypatch.setattr(build_paths_module, "_discovery_batch_size", lambda: 3)

    sleeps: list[float] = []
    real_sleep = asyncio.sleep

    async def counting_sleep(delay: float = 0) -> None:
        sleeps.append(delay)
        await real_sleep(0)

    monkeypatch.setattr(_pathfinding.asyncio, "sleep", counting_sleep)

    got = [path async for path in _make_pipeline().discovery_sweep()]
    assert got == [[f"p{i}"] for i in range(9)]
    assert len(sleeps) == 3, "9 paths / batch_size 3 == 3 event-loop hops"
