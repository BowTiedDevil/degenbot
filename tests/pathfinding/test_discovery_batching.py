"""Discovery delivery batching (4IOEVT): red/green behavioral tests.

find_paths_async is a thin async adapter over the Rust batched async iterator
(`degenbot._ffi.find_paths_async_rust` -> `PathBatchIterator`): the DFS runs
on the shared tokio runtime and is driven from Rust, one `__anext__` per
batch. These tests pin:

- path-output parity vs the sync find_paths stream (batch_size 1 / small /
  default / oversized),
- the Rust iterator's batch cadence (each `__anext__` yields <= batch_size
  paths; one batch == one event-loop hop),
- cancellation (aclose) releasing the Rust batch iterator with no stray
  worker threads,
- producer (Rust `__anext__`) exceptions re-raising at the consumer after
  the pending batch drains,
- the typed pathfinding.discovery_batch_size config key plumbing.
"""

from __future__ import annotations

import asyncio
import contextlib
import gc
import os
import pathlib
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import threading
import time
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import (
    PathfindingRequest,
    PoolKind,
    _pathfinding,
    find_paths,
    find_paths_async,
    find_paths_async_rust,
    find_paths_rust,
)
from degenbot.runner.build_paths import PathRegistrationPipeline
from degenbot.types.chain import ChainId
from tests.helpers.database import seed_v2_topology

if TYPE_CHECKING:
    from collections.abc import AsyncGenerator

CHAIN = ChainId.ETH
WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN_A_ADDR = "0x" + "AA" * 20
POOL_A_ADDR = "0x" + "11" * 20
POOL_B_ADDR = "0x" + "12" * 20


def _base_request(database_path: pathlib.Path) -> PathfindingRequest:
    """Build the shared two-pool synthetic search request for the parity tests."""
    return PathfindingRequest(
        chain_id=CHAIN,
        start_tokens=[WETH_ADDR],
        end_tokens=[WETH_ADDR],
        max_depth=2,
        pool_types=[PoolKind.V2],
        database_path=database_path,
    )


def _adapter_request() -> PathfindingRequest:
    """Build a path-only request for tests that replace the prep seam."""
    return PathfindingRequest(
        chain_id=1,
        start_tokens=[],
        end_tokens=[],
        database_path=pathlib.Path("unused.db"),
    )


def _seed_two_pool_db(db_path: pathlib.Path) -> pathlib.Path:
    """Seed a file-backed temp SQLite with a WETH<->A two-pool cycle."""
    seed_v2_topology(
        db_path,
        [
            (POOL_A_ADDR, WETH_ADDR, TOKEN_A_ADDR),
            (POOL_B_ADDR, TOKEN_A_ADDR, WETH_ADDR),
        ],
        chain_id=CHAIN,
    )
    return db_path


@pytest.fixture
def db(tmp_path: pathlib.Path) -> pathlib.Path:
    return _seed_two_pool_db(tmp_path / "discovery_batching.db")


def _path_signature(paths: list[object]) -> list[tuple[tuple[str, str | None, object], ...]]:
    return [
        tuple((step.address, step.hash, step.type) for step in path)  # type: ignore[attr-defined]
        for path in paths
    ]


async def _collect(**kwargs: object) -> list[object]:
    return [path async for path in find_paths_async(**kwargs)]  # type: ignore[arg-type]


async def _drain_into(producer: AsyncGenerator[object, None], sink: list[object]) -> None:
    """Stream ``producer`` into ``sink`` one item at a time.

    A list comprehension would be eager: an exception would discard the items
    already yielded. The test asserts pending items are delivered before the
    producer's raise, so the sink must be mutated incrementally.
    """
    async for path in producer:
        sink.append(path)  # ruff: ignore[manual-list-comprehension]


# ---------------------------------------------------------------------------
# Parity: the async batch stream must be byte-identical to the sync stream.
# ---------------------------------------------------------------------------


def test_batched_async_matches_sync_stream(db: pathlib.Path) -> None:
    """Content + order parity: 1, small, default, and oversized batches."""
    expected = list(find_paths(request=_base_request(db)))
    assert expected, "fixture graph produced no paths"

    for batch_size in (1, 2, 1000, 10**6):
        got = asyncio.run(_collect(request=_base_request(db), batch_size=batch_size))
        assert _path_signature(got) == _path_signature(expected), (
            f"batched stream diverged at batch_size={batch_size}"
        )


# ---------------------------------------------------------------------------
# The Rust batched async iterator (direct seam).
# ---------------------------------------------------------------------------


def _star_edges() -> list[tuple[int, int, int, PoolKind]]:
    """Three parallel WETH(1)<->A(2) pools each way -> 3 * 3 == 9 cycles."""
    forward = [(1, 2, 100 + i, PoolKind.V2) for i in range(3)]
    reverse = [(2, 1, 200 + i, PoolKind.V2) for i in range(3)]
    return [*forward, *reverse]


async def _collect_batches(
    edges: list[tuple[int, int, int, PoolKind]], batch_size: int
) -> tuple[list[object], list[int]]:
    iterator = find_paths_async_rust(
        edges,
        1,
        1,
        2,
        2,
        include_reverse=False,
        pool_type_per_depth=None,
        batch_size=batch_size,
    )
    batches = [batch async for batch in iterator]
    paths = [path for batch in batches for path in batch]
    return paths, [len(batch) for batch in batches]


def test_rust_async_iterator_yields_batches_of_at_most_batch_size() -> None:
    """One `__anext__` returns <= batch_size paths; the union is the DFS."""
    edges = _star_edges()
    expected = list(
        find_paths_rust(edges, 1, 1, 2, 2, include_reverse=False, pool_type_per_depth=None)
    )
    # Each undirected tuple becomes both directions, so 3x3 forward x reverse
    # pool pairs minus the 6 same-pool repeats == 30 two-hop cycles.
    assert len(expected) == 30, "fixture must yield 30 two-hop cycles"

    for batch_size in (1, 2, 4, 1000):
        got, sizes = asyncio.run(_collect_batches(edges, batch_size))
        assert got == expected, f"batch stream diverged at batch_size={batch_size}"
        assert all(size <= batch_size for size in sizes), sizes
        assert sum(sizes) == 30


def test_rust_async_iterator_batches_cadence() -> None:
    """batch_size=4 over 30 paths -> 7 batches of 4 + one of 2."""
    _got, sizes = asyncio.run(_collect_batches(_star_edges(), 4))
    assert sizes == [4, 4, 4, 4, 4, 4, 4, 2]


def test_rust_async_iterator_rejects_bad_pool_kind() -> None:
    """The typed seam rejects a bare int where a `PoolKind` is required."""
    with pytest.raises(TypeError):
        find_paths_async_rust(
            [(1, 2, 1, 3)],  # type: ignore[list-item]
            1,
            1,
            2,
            2,
            include_reverse=False,
            pool_type_per_depth=None,
            batch_size=10,
        )


# ---------------------------------------------------------------------------
# The thin Python adapter (fake Rust seam, no DB needed).
# ---------------------------------------------------------------------------


class _FakeBatchIterator:
    """A deterministic async iterator standing in for the Rust batch seam."""

    def __init__(
        self,
        batches: list[list[object]],
        *,
        error: BaseException | None = None,
        dropped: list[bool] | None = None,
    ) -> None:
        self._batches = list(batches)
        self._error = error
        self._dropped = dropped
        self._index = 0
        self.calls = 0

    def __aiter__(self) -> _FakeBatchIterator:
        return self

    async def __anext__(self) -> list[object]:
        self.calls += 1
        await asyncio.sleep(0)
        if self._index >= len(self._batches):
            if self._error is not None:
                exc, self._error = self._error, None
                raise exc
            raise StopAsyncIteration
        batch = self._batches[self._index]
        self._index += 1
        return batch

    def __del__(self) -> None:
        if self._dropped is not None:
            self._dropped.append(True)


class _BoomError(RuntimeError):
    pass


class _FakeStepBuilder:
    """Pass-through standing in for the Rust `PathStepBuilder`."""

    def build(self, raw_path: list[object]) -> list[object]:
        return raw_path


def _fake_traversal() -> object:
    prepared = _pathfinding._PreparedGraph(edges=[], step_builder=_FakeStepBuilder())
    return _pathfinding._Traversal(
        prepared=prepared,
        start_token_id=1,
        end_token_id=1,
        include_reverse=False,
        min_depth=2,
        pool_kind_filter=None,
    )


def _install_fake_rust_seam(
    monkeypatch: pytest.MonkeyPatch,
    *,
    batches: list[list[object]],
    error: BaseException | None = None,
    dropped: list[bool] | None = None,
    captured: dict[str, object] | None = None,
) -> None:
    """Patch the prep + Rust batch seam so no DB is required."""

    def factory(*args: object, **kwargs: object) -> _FakeBatchIterator:
        if captured is not None:
            captured["args"] = args
            captured["kwargs"] = kwargs
        return _FakeBatchIterator(batches, error=error, dropped=dropped)

    monkeypatch.setattr(_pathfinding, "find_paths_async_rust", factory)
    monkeypatch.setattr(_pathfinding, "_prepare_traversals", lambda **_: [_fake_traversal()])


def test_adapter_forwards_batch_size_to_rust_seam(monkeypatch: pytest.MonkeyPatch) -> None:
    """The adapter hands the requested batch_size straight to the Rust seam."""
    captured: dict[str, object] = {}
    _install_fake_rust_seam(monkeypatch, batches=[[[], []]], captured=captured)

    got = asyncio.run(
        _collect(
            request=_adapter_request(),
            batch_size=7,
        )
    )
    assert got == [[], []]

    args = captured["args"]
    assert isinstance(args, tuple)
    assert args[0] == []  # edges from the (fake) prepared graph
    assert args[7] == 7  # batch_size


async def test_producer_exception_reraises_at_consumer(monkeypatch: pytest.MonkeyPatch) -> None:
    """A Rust `__anext__` failure surfaces at the consumer, pending batch first."""
    _install_fake_rust_seam(
        monkeypatch,
        batches=[[[], []]],
        error=_BoomError("producer died"),
    )

    got: list[object] = []
    with pytest.raises(_BoomError, match="producer died"):
        await _drain_into(
            find_paths_async(
                request=_adapter_request(),
                batch_size=2,
            ),
            got,
        )

    assert got == [[], []], "the pending batch must be delivered before the raise"


async def test_aclose_releases_the_rust_iterator(monkeypatch: pytest.MonkeyPatch) -> None:
    """aclose clears the adapter's reference so the Rust iterator can drop."""
    dropped: list[bool] = []
    _install_fake_rust_seam(
        monkeypatch,
        batches=[[[], [], []], [[], [], []]],
        dropped=dropped,
    )

    agen = find_paths_async(request=_adapter_request(), batch_size=2)
    seen = 0
    async for _path in agen:
        seen += 1
        break
    await agen.aclose()
    gc.collect()

    assert seen == 1
    assert dropped, "the adapter must release the Rust batch iterator on aclose"


async def _partial_sweep(db: pathlib.Path, *, take: int) -> int:
    agen = find_paths_async(request=_base_request(db))
    seen = 0
    async for _path in agen:
        seen += 1
        if seen >= take:
            break
    await agen.aclose()
    return seen


def _persistent_threads() -> dict[int, str]:
    """Snapshot the persistent, named threads by ident.

    `threading.enumerate()` / `active_count()` also report CPython *dummy*
    threads: native threads that merely held the GIL at the instant of the
    snapshot. The `rust-log-drainer` bridge (`Python::attach` -> Python
    logging, which reads `threading.current_thread()`) and the ambient
    runtime's blocking pool both register a transient `Dummy-N` this way.
    They are bookkeeping for a foreign thread, never a leaked Python worker,
    so a raw count is racy; a real leak is a persistent thread with its own
    name (`degenbot-discovery`, executor `asyncio_*`, ...).
    """
    return {
        t.ident: t.name
        for t in threading.enumerate()
        if t.ident is not None and not t.name.startswith("Dummy-")
    }


async def test_aclose_leaves_no_worker_threads(db: pathlib.Path) -> None:
    """Repeated mid-sweep closes leave no stray discovery worker threads."""
    # Warm any lazily-created runtime/executor threads first.
    await _partial_sweep(db, take=1)
    await asyncio.sleep(0.2)
    baseline = _persistent_threads()

    for _ in range(6):
        await _partial_sweep(db, take=1)

    assert not any(t.name == "degenbot-discovery" for t in threading.enumerate())
    leaked = {ident: name for ident, name in _persistent_threads().items() if ident not in baseline}
    assert not leaked, f"stray threads: {leaked} (baseline {baseline})"


# ---------------------------------------------------------------------------
# Prep is lifted off the event loop (FYZMAF).
# ---------------------------------------------------------------------------


async def test_prep_never_blocks_the_event_loop(monkeypatch: pytest.MonkeyPatch) -> None:
    """A slow prep must not stall the asyncio loop (canary keeps ticking).

    Pre-FYZMAF the one-time prep (`_prepare_traversals`: Rust token
    resolution + the `build_path_graph` bulk read) ran INLINE on the event
    loop at first `__anext__`, so a canary coroutine made no progress for the
    whole prep. It now runs on the Rust async seam's blocking pool, so the
    loop keeps turning while the prep is in flight.
    """
    ticks = 0

    async def canary() -> None:
        nonlocal ticks
        while True:
            await asyncio.sleep(0.01)
            ticks += 1

    def slow_prep(**_: object) -> list[object]:
        time.sleep(0.3)
        return [_fake_traversal()]

    monkeypatch.setattr(_pathfinding, "_prepare_traversals", slow_prep)
    monkeypatch.setattr(
        _pathfinding, "find_paths_async_rust", lambda *a, **k: _FakeBatchIterator([[]])
    )

    canary_task = asyncio.ensure_future(canary())
    agen = find_paths_async(request=_adapter_request())
    started = time.perf_counter()
    with pytest.raises(StopAsyncIteration):
        await anext(agen)
    elapsed = time.perf_counter() - started
    canary_task.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await canary_task

    assert elapsed >= 0.3, "the slow prep did not actually run"
    # One event-loop turn is the invariant: the offloaded prep lets the canary
    # run at least once, while an inline blocking prep yields zero before the
    # cancellation below. The exact count is scheduler-dependent under xdist.
    assert ticks >= 1, f"event loop stalled during prep: {ticks} canary ticks in {elapsed:.2f}s"


async def test_missing_start_token_raises_at_first_next(db: pathlib.Path) -> None:
    """Token-resolution errors keep today's lazy DegenbotValueError shape.

    The prep (and therefore the DB token lookup) runs at first `__anext__`,
    not at generator construction, and a missing boundary token raises the
    same `DegenbotValueError` the sync path raises — now from the Rust
    blocking seam.
    """
    agen = find_paths_async(
        request=PathfindingRequest(
            database_path=db,
            chain_id=CHAIN,
            start_tokens=[ZERO_ADDRESS],
            end_tokens=[WETH_ADDR],
            max_depth=2,
            pool_types=[PoolKind.V2],
        )
    )
    with pytest.raises(DegenbotValueError, match="was not found in the database"):
        await anext(agen)


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
        database_path=pathlib.Path("unused.db"),
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
