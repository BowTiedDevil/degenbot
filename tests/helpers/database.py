"""Small Rust-owned database fixture helpers for Python tests."""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING

from degenbot.db import (
    db_create_new_database,
    db_set_exchange_active,
    db_upsert_exchange,
    db_upsert_v2_pools,
)
from degenbot.updater import V2PoolRowInput

if TYPE_CHECKING:
    from collections.abc import Sequence

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
    db_create_new_database(str(database_path))
    exchange = db_upsert_exchange(
        database_path=str(database_path),
        chain_id=chain_id,
        name=exchange_name,
        factory=ZERO_ADDRESS,
        deployer=None,
    )
    db_set_exchange_active(
        database_path=str(database_path),
        exchange_id=exchange.id,
        active=True,
    )
    db_upsert_v2_pools(
        database_path=str(database_path),
        chain_id=chain_id,
        kind=kind,
        exchange_id=exchange.id,
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
