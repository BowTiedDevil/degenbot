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
