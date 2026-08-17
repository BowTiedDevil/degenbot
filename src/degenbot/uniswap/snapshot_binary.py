"""Snapshot → Python-dict converters for the non-DB path.

The DB→ ``SnapshotStore`` transfer is Rust-owned: the DB snapshot is loaded
inside ``Bot::load_snapshot_from_db`` (task B3OROH) at ``RustBot`` construction
— zero tick-data dicts cross PyO3. The DB-source branch that used to live
here (SQLAlchemy ``yield_per`` loops calling ``engine.insert_*_pool_snapshot``
per pool) is retired (DADWUP): the per-pool PyO3 ingestion surface
(``begin_*_snapshot_stream`` / ``insert_*_pool_snapshot`` / ``finish_*_snapshot``)
is gone.

What remains here is the **non-DB path**: converters that walk an in-memory /
file-backed ``UniswapV3LiquiditySnapshot`` / ``UniswapV4LiquiditySnapshot`` and
build a single Python dict, which the caller hands to the engine via
``engine.load_v3_snapshot_from_py(...)`` / ``load_v4_snapshot_from_py(...)`` —
ONE PyO3 crossing per family with the whole snapshot. ``engine_registry.start``
uses these for the non-DB ``v3_snapshot`` / ``v4_snapshot`` kwargs.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from hexbytes import HexBytes

from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot

if TYPE_CHECKING:
    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
    from degenbot.uniswap.v4_snapshot import ManagedPoolIdentifier, UniswapV4LiquiditySnapshot


def _prime_v3_snapshot(snapshot: UniswapV3LiquiditySnapshot) -> None:
    """Eagerly populate the lazy KeyedDefaultDict by accessing each pool.

    The snapshot's _liquidity_snapshot is a KeyedDefaultDict that lazily
    populates entries on __getitem__. We must access each pool address
    before iterating to ensure all entries are present.
    """
    for pool_address in snapshot.pools:
        _ = snapshot._liquidity_snapshot[pool_address]  # ruff:ignore[private-member-access]


def _prime_v4_snapshot(
    snapshot: UniswapV4LiquiditySnapshot,
    managed_pools: set[ManagedPoolIdentifier],
) -> None:
    """Eagerly populate the lazy KeyedDefaultDict by accessing each pool.

    The V4 snapshot's _liquidity_snapshot is keyed by (pool_manager, pool_id).
    Unlike V3, we can't enumerate pools from snapshot.pools (it's empty until
    accessed). The caller must provide the set of (pool_manager, pool_id) pairs.
    """
    for pool_manager, pool_id in managed_pools:
        _ = snapshot._liquidity_snapshot[pool_manager, pool_id]  # ruff:ignore[private-member-access]


def _normalize_v4_pool_id(pool_id: str | bytes) -> str:
    """Normalize a V4 pool_id to a 0x-prefixed hex string.

    Returns:
        The pool_id as a 0x-prefixed lowercase hex string.

    """
    if isinstance(pool_id, bytes):
        return HexBytes(pool_id).to_0x_hex()
    if pool_id.startswith("0x"):
        return pool_id
    return HexBytes(pool_id).to_0x_hex()


def _v3_snapshot_to_py_dict(
    snapshot: UniswapV3LiquiditySnapshot,
) -> dict[str, dict[int, tuple[int, int]]]:
    """Convert a V3 snapshot to a dict suitable for ``load_v3_snapshot_from_py()``.

    For a ``DatabaseSnapshot`` source, returns the raw-SQL batch dict directly
    (``{addr: {tick: (lg, ln)}}``). For in-memory sources, primes the lazy
    snapshot dict and converts the Pydantic models.

    The returned dict crosses PyO3 ONCE (no per-pool crossings). Feed it to
    ``engine.load_v3_snapshot_from_py(...)``.

    Returns:
        A dict mapping pool addresses to tick data dicts.

    """
    if isinstance(snapshot._source, V3DatabaseSnapshot):  # ruff:ignore[private-member-access]
        # Batch raw SQL path — already returns {addr: {tick: (lg, ln)}}
        return {str(k): v for k, v in snapshot._source.get_all_liquidity_maps().items()}  # ruff:ignore[private-member-access]

    # Fallback: prime the lazy dict and convert from Pydantic models
    _prime_v3_snapshot(snapshot)
    liquidity_snapshot = snapshot._liquidity_snapshot  # ruff:ignore[private-member-access]

    result: dict[str, dict[int, tuple[int, int]]] = {}
    for pool_address, pool_mapping in liquidity_snapshot.items():
        if pool_mapping is None:
            result[pool_address] = {}
            continue
        tick_data: dict[int, tuple[int, int]] = {}
        for tick_index, liquidity_at_tick in pool_mapping["tick_data"].items():
            tick_data[tick_index] = (
                liquidity_at_tick.liquidity_gross,
                liquidity_at_tick.liquidity_net,
            )
        result[pool_address] = tick_data
    return result


def _v4_snapshot_to_py_dict(
    snapshot: UniswapV4LiquiditySnapshot,
    managed_pools: set[ManagedPoolIdentifier] | None = None,
) -> dict[str, dict[str, dict[int, tuple[int, int]]]]:
    """Convert a V4 snapshot to a dict suitable for ``load_v4_snapshot_from_py()``.

    For a ``DatabaseSnapshot`` source, returns the raw-SQL batch dict directly.
    For in-memory sources, primes the lazy snapshot dict and converts the
    Pydantic models.

    The returned dict crosses PyO3 ONCE (no per-pool crossings). Feed it to
    ``engine.load_v4_snapshot_from_py(...)``.

    Returns:
        A dict mapping pool manager addresses to pool ID dicts.

    """
    if isinstance(snapshot._source, V4DatabaseSnapshot):  # ruff:ignore[private-member-access]
        # Batch raw SQL path — returns {(pm_addr, pool_id_hex): {tick: (lg, ln)}}
        all_maps = snapshot._source.get_all_liquidity_maps()  # ruff:ignore[private-member-access]
        result: dict[str, dict[str, dict[int, tuple[int, int]]]] = {}
        for (pm_addr, pool_id_hex), tick_data in all_maps.items():
            if pm_addr not in result:
                result[pm_addr] = {}
            result[pm_addr][pool_id_hex] = tick_data
        return result

    # Fallback: prime the lazy dict and convert from Pydantic models
    _prime_v4_snapshot(snapshot, managed_pools if managed_pools is not None else snapshot.pools)
    liquidity_snapshot = snapshot._liquidity_snapshot  # ruff:ignore[private-member-access]

    result: dict[str, dict[str, dict[int, tuple[int, int]]]] = {}
    for (pm_addr, pool_id), pool_mapping in liquidity_snapshot.items():
        if pm_addr not in result:
            result[pm_addr] = {}
        pool_id_hex = _normalize_v4_pool_id(pool_id)
        if pool_mapping is None:
            result[pm_addr][pool_id_hex] = {}
            continue
        tick_data: dict[int, tuple[int, int]] = {}
        for tick_index, liquidity_at_tick in pool_mapping["tick_data"].items():
            tick_data[tick_index] = (
                liquidity_at_tick.liquidity_gross,
                liquidity_at_tick.liquidity_net,
            )
        result[pm_addr][pool_id_hex] = tick_data
    return result
