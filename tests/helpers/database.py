"""Small Rust-owned database fixture helpers for Python tests."""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from typing import TYPE_CHECKING

from degenbot.db import (
    db_set_exchange_active,
    db_upgrade_database,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.updater import V2PoolRowInput, V3PoolRowInput, V4PoolRowInput

if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

ZERO_ADDRESS = "0x0000000000000000000000000000000000000000"


@contextmanager
def sqlite_connection(database_path: Path):
    """Own one sqlite connection, commit on success, and always close it."""
    connection = sqlite3.connect(database_path)
    try:
        with connection:
            yield connection
    finally:
        connection.close()


def _v2_exchange(database_path: Path, *, chain_id: int, name: str) -> int:
    """Get-or-create + activate an exchange row, returning its id."""
    exchange = db_upsert_exchange(
        database_path=str(database_path),
        chain_id=chain_id,
        name=name,
        factory=ZERO_ADDRESS,
        deployer=None,
    )
    db_set_exchange_active(
        database_path=str(database_path),
        exchange_id=exchange.id,
        active=True,
    )
    return exchange.id


def seed_v2_topology(
    database_path: Path,
    pools: Sequence[tuple[str, str, str]],
    *,
    chain_id: int = 1,
    exchange_name: str = "test",
    fee_token0: int = 3,
    fee_token1: int = 3,
    fee_denominator: int = 1000,
    kind: str = "uniswap_v2",
) -> None:
    """Create a Rust-owned database and write a V2 topology through Rust seams."""
    db_upgrade_database(str(database_path))
    exchange_id = _v2_exchange(database_path, chain_id=chain_id, name=exchange_name)
    db_upsert_v2_pools(
        database_path=str(database_path),
        chain_id=chain_id,
        kind=kind,
        exchange_id=exchange_id,
        fee_denominator=fee_denominator,
        rows=[
            V2PoolRowInput(
                address=pool_address,
                token0_address=token0,
                token1_address=token1,
                fee_token0=fee_token0,
                fee_token1=fee_token1,
            )
            for pool_address, token0, token1 in pools
        ],
    )


def seed_v3_topology(
    database_path: Path,
    pools: Sequence[tuple[str, str, str, int, int]],
    *,
    chain_id: int = 1,
    exchange_name: str = "uniswap_v3",
    fee_denominator: int = 1_000_000,
    kind: str = "uniswap_v3",
) -> None:
    """Write a V3 topology through Rust seams into a database.

    ``pools`` rows are ``(address, token0, token1, fee, tick_spacing)``.
    ``db_upgrade_database`` runs first, so a caller may compose a mixed
    V2 + V3 + V4 topology by chaining the ``seed_*_topology`` helpers.
    """
    db_upgrade_database(str(database_path))
    exchange_id = _v2_exchange(database_path, chain_id=chain_id, name=exchange_name)
    db_upsert_v3_pools(
        database_path=str(database_path),
        chain_id=chain_id,
        kind=kind,
        exchange_id=exchange_id,
        fee_denominator=fee_denominator,
        rows=[
            V3PoolRowInput(
                address=pool_address,
                token0_address=token0,
                token1_address=token1,
                fee=fee,
                tick_spacing=tick_spacing,
            )
            for pool_address, token0, token1, fee, tick_spacing in pools
        ],
    )


def seed_v4_topology(
    database_path: Path,
    pools: Sequence[tuple[str, str, str, str, int]],
    *,
    pool_manager_address: str,
    chain_id: int = 1,
    exchange_name: str = "uniswap_v4",
    fee_denominator: int = 1_000_000,
) -> None:
    """Write a V4 topology through Rust seams into a database.

    ``pools`` rows are ``(pool_hash, currency0, currency1, hooks, fee)``. A
    ``PoolManager`` row is created for ``pool_manager_address`` first (the V4
    upsert resolves the manager by ``(chain, address)``). ``db_upgrade_database``
    runs first, so the V4 helper composes with the V2/V3 helpers for a mixed
    topology.
    """
    db_upgrade_database(str(database_path))
    exchange_id = _v2_exchange(database_path, chain_id=chain_id, name=exchange_name)
    db_upsert_pool_manager(
        database_path=str(database_path),
        address=pool_manager_address,
        chain=chain_id,
        kind="uniswap_v4",
        state_view=None,
        exchange_id=exchange_id,
    )
    db_upsert_v4_pools(
        database_path=str(database_path),
        chain_id=chain_id,
        pool_manager_address=pool_manager_address,
        fee_denominator=fee_denominator,
        rows=[
            V4PoolRowInput(
                pool_hash=pool_hash,
                hooks=hooks,
                currency0_address=currency0,
                currency1_address=currency1,
                fee=fee,
                tick_spacing=60,
            )
            for pool_hash, currency0, currency1, hooks, fee in pools
        ],
    )


def seed_liquidity_positions(
    database_path: Path,
    rows: Sequence[tuple[int, int, int]],
    *,
    chain_id: int = 1,
    pool_address: str | None = None,
    pool_hash: str | None = None,
    stamp: tuple[int, int] | None = None,
) -> None:
    """Seed a pool's committed liquidity rows + update marker.

    ``rows`` are ``(tick, liquidity_net, liquidity_gross)`` triples written

    verbatim into the family's position table — the fixture states a KNOWN map,

    it never re-derives one (the apply math is the core's, and a test that

    feeds the core its own output proves nothing). ``stamp`` is the pool's

    ``(liquidity_update_block, liquidity_update_log_index)`` marker, written

    when given.

    Exactly one of ``pool_address`` (V3) / ``pool_hash`` (V4) names the pool.

    """

    if (pool_address is None) == (pool_hash is None):
        msg = "seed_liquidity_positions needs exactly one of pool_address / pool_hash"

        raise ValueError(msg)

    with sqlite_connection(database_path) as connection:
        if pool_address is not None:
            pool_id = connection.execute(
                "SELECT id FROM pools WHERE address = ? AND chain = ?",
                (pool_address, chain_id),
            ).fetchone()[0]

            table, id_column, subclass, subclass_column = (
                "liquidity_positions",
                "pool_id",
                "uniswap_v3_pools",
                "pool_id",
            )

        else:
            pool_id = connection.execute(
                "SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash = ?",
                (pool_hash,),
            ).fetchone()[0]

            table, id_column, subclass, subclass_column = (
                "managed_pool_liquidity_positions",
                "managed_pool_id",
                "uniswap_v4_pools",
                "managed_pool_id",
            )

        connection.executemany(
            f"INSERT INTO {table} ({id_column}, tick, liquidity_net, liquidity_gross) "
            "VALUES (?, ?, ?, ?)",
            [(pool_id, tick, str(net), str(gross)) for tick, net, gross in rows],
        )

        if stamp is not None:
            block, log_index = stamp

            connection.execute(
                f"UPDATE {subclass} SET liquidity_update_block = ?, "
                f"liquidity_update_log_index = ? WHERE {subclass_column} = ?",
                (block, log_index, pool_id),
            )
