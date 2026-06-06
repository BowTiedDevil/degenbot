"""Streaming snapshot loaders for V3/V4 liquidity snapshots.

Streams tick data from the database into the Rust engine one pool at a time,
avoiding the need to materialize the entire snapshot as a Python dict.

For DatabaseSnapshot sources, uses SQLAlchemy yield_per() to stream rows
from the DB, grouping by pool without loading the full result set. Each pool
is inserted via engine.insert_v3_pool_snapshot() / insert_v4_pool_snapshot().

For non-DatabaseSnapshot sources, falls back to batch loading via
engine.load_v3_snapshot_from_py() / load_v4_snapshot_from_py().
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from hexbytes import HexBytes
from sqlalchemy import text as sa_text

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
        _ = snapshot._liquidity_snapshot[pool_address]  # noqa: SLF001


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
        _ = snapshot._liquidity_snapshot[(pool_manager, pool_id)]  # noqa: SLF001


def _normalize_v4_pool_id(pool_id: str | bytes) -> str:
    """Normalize a V4 pool_id to a 0x-prefixed hex string."""
    if isinstance(pool_id, bytes):
        return HexBytes(pool_id).to_0x_hex()
    if pool_id.startswith("0x"):
        return pool_id
    return HexBytes(pool_id).to_0x_hex()


def _v3_snapshot_to_py_dict(
    snapshot: UniswapV3LiquiditySnapshot,
) -> dict[str, dict[int, tuple[int, int]]]:
    """Convert a V3 snapshot to a dict suitable for load_v3_snapshot_from_py().

    Returns {pool_address_hex: {tick_index: (liquidity_gross, liquidity_net)}}.
    """
    if isinstance(snapshot._source, V3DatabaseSnapshot):  # noqa: SLF001
        # Batch raw SQL path — already returns {addr: {tick: (lg, ln)}}
        return snapshot._source.get_all_liquidity_maps()  # noqa: SLF001

    # Fallback: prime the lazy dict and convert from Pydantic models
    _prime_v3_snapshot(snapshot)
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001

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
    managed_pools: set[ManagedPoolIdentifier],
) -> dict[str, dict[str, dict[int, tuple[int, int]]]]:
    """Convert a V4 snapshot to a dict suitable for load_v4_snapshot_from_py().

    Returns {pool_manager_hex: {pool_id_hex: {tick_index: (lg, ln)}}}.
    """
    if isinstance(snapshot._source, V4DatabaseSnapshot):  # noqa: SLF001
        # Batch raw SQL path — returns {(pm_addr, pool_id_hex): {tick: (lg, ln)}}
        all_maps = snapshot._source.get_all_liquidity_maps()  # noqa: SLF001
        result: dict[str, dict[str, dict[int, tuple[int, int]]]] = {}
        for (pm_addr, pool_id_hex), tick_data in all_maps.items():
            if pm_addr not in result:
                result[pm_addr] = {}
            result[pm_addr][pool_id_hex] = tick_data
        return result

    # Fallback: prime the lazy dict and convert from Pydantic models
    _prime_v4_snapshot(snapshot, managed_pools)
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001

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


def stream_v3_snapshot_to_engine(
    snapshot: UniswapV3LiquiditySnapshot,
    engine: object,
) -> None:
    """Stream V3 tick data from the DB into the Rust engine, one pool at a time.

    Uses SQLAlchemy yield_per() to stream rows from the DB, grouping by pool
    address without materializing the entire result set. Each pool is inserted
    via engine.insert_v3_pool_snapshot() — no giant Python dict is ever held.

    Falls back to load_v3_snapshot_from_py() for non-DatabaseSnapshot sources.
    """
    if not isinstance(snapshot._source, V3DatabaseSnapshot):  # noqa: SLF001
        # Non-DB source — fall back to batch method
        engine.load_v3_snapshot_from_py(_v3_snapshot_to_py_dict(snapshot))
        return

    engine.begin_v3_snapshot_stream()

    session = snapshot._source.session  # noqa: SLF001
    current_addr: str | None = None
    current_ticks: dict[int, tuple[int, int]] = {}

    rows = session.execute(
        sa_text(
            """
            SELECT p.address, lp.tick, lp.liquidity_gross, lp.liquidity_net
            FROM pools p
            JOIN liquidity_positions lp ON lp.pool_id = p.id
            WHERE p.chain = :chain_id
              AND p.kind IN ('uniswap_v3', 'sushiswap_v3', 'pancakeswap_v3', 'aerodrome_v3')
            ORDER BY p.address, lp.tick
            """
        ),
        {"chain_id": snapshot.chain_id},
    ).yield_per(10_000)

    for pool_address, tick, liquidity_gross, liquidity_net in rows:
        if pool_address != current_addr:
            # Flush previous pool
            if current_addr is not None and current_ticks:
                engine.insert_v3_pool_snapshot(current_addr, current_ticks)
            current_addr = pool_address
            current_ticks = {}
        current_ticks[int(tick)] = (int(liquidity_gross), int(liquidity_net))

    # Flush last pool
    if current_addr is not None and current_ticks:
        engine.insert_v3_pool_snapshot(current_addr, current_ticks)

    engine.finish_v3_snapshot()


def stream_v4_snapshot_to_engine(
    snapshot: UniswapV4LiquiditySnapshot,
    engine: object,
) -> None:
    """Stream V4 tick data from the DB into the Rust engine, one pool at a time.

    Uses SQLAlchemy yield_per() to stream rows from the DB, grouping by
    (pool_manager, pool_id) without materializing the entire result set.

    Falls back to load_v4_snapshot_from_py() for non-DatabaseSnapshot sources.
    """
    if not isinstance(snapshot._source, V4DatabaseSnapshot):  # noqa: SLF001
        # Non-DB source — fall back to batch method
        engine.load_v4_snapshot_from_py(
            _v4_snapshot_to_py_dict(snapshot, managed_pools=snapshot.pools)
        )
        return

    engine.begin_v4_snapshot_stream()

    session = snapshot._source.session  # noqa: SLF001
    current_key: tuple[str, str] | None = None
    current_ticks: dict[int, tuple[int, int]] = {}

    rows = session.execute(
        sa_text(
            """
            SELECT pm.address, v4.pool_hash, lp.tick, lp.liquidity_gross, lp.liquidity_net
            FROM pool_managers pm
            JOIN managed_pools mp ON mp.manager_id = pm.id
            JOIN uniswap_v4_pools v4 ON v4.managed_pool_id = mp.id
            JOIN managed_pool_liquidity_positions lp ON lp.managed_pool_id = mp.id
            WHERE pm.chain = :chain_id AND mp.kind = 'uniswap_v4'
            ORDER BY pm.address, v4.pool_hash, lp.tick
            """
        ),
        {"chain_id": snapshot.chain_id},
    ).yield_per(10_000)

    for pm_address, pool_hash, tick, liquidity_gross, liquidity_net in rows:
        key = (pm_address, pool_hash)
        if key != current_key:
            # Flush previous pool
            if current_key is not None and current_ticks:
                engine.insert_v4_pool_snapshot(current_key[0], current_key[1], current_ticks)
            current_key = key
            current_ticks = {}
        current_ticks[int(tick)] = (int(liquidity_gross), int(liquidity_net))

    # Flush last pool
    if current_key is not None and current_ticks:
        engine.insert_v4_pool_snapshot(current_key[0], current_key[1], current_ticks)

    engine.finish_v4_snapshot()
