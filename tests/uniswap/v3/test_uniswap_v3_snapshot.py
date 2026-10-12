"""V3 liquidity-snapshot read-surface tests over the database source.

The file- and directory-backed JSON snapshot sources were removed: they were
Python-only, with no Rust counterpart (ADR-005 feature parity). The facade's
drain-on-read / merge / event semantics are pinned hermetically by
``tests/uniswap/test_snapshot_facade_semantics.py``; what remains here is the
database source wiring (which routes through the Rust ``degenbot-db`` core
crate) and its read surface.

The database is seeded hermetically from python through the ``degenbot.db``
write seam (exchange upsert -> last-update-block stamp -> pool rows ->
liquidity events): no RPC, no fork, and no shipped database file. The first
write op creates and migrates the schema at the given path; the block stamp
satisfies the snapshot initializer's ``newest_block`` gate.
"""

from collections.abc import Iterator
from pathlib import Path

import pytest

from degenbot._ffi.db import LiquidityUpdateEvent, V3PoolRowInput
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.db import (
    db_apply_v3_liquidity_updates,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
    db_upsert_v3_pools,
)
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot, UniswapV3LiquiditySnapshot

CHAIN_ID = 8453
POOL_ADDRESS = get_checksum_address("0xe13514aacc27a3dfd2ae0db6ada4ef7658c1e435")
TOKEN0_ADDRESS = get_checksum_address("0x4200000000000000000000000000000000000006")
TOKEN1_ADDRESS = get_checksum_address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
FACTORY_ADDRESS = get_checksum_address("0x248ab47bdb8174285daca662ac4fc41e71660cc5")
SEED_BLOCK = 1_000


def _seed_exchange(database_path: Path) -> int:
    """Create the schema at the path and an initialized exchange row."""
    exchange = db_upsert_exchange(str(database_path), CHAIN_ID, "uniswap_v3", FACTORY_ADDRESS, None)
    db_set_exchange_last_update_block(str(database_path), CHAIN_ID, exchange.id, SEED_BLOCK)
    return exchange.id


def _seed_pool(database_path: Path, exchange_id: int) -> None:
    """Insert one V3 pool row and one in-range liquidity position."""
    db_upsert_v3_pools(
        str(database_path),
        CHAIN_ID,
        "uniswap_v3",
        exchange_id,
        10000,
        [V3PoolRowInput(POOL_ADDRESS, TOKEN0_ADDRESS, TOKEN1_ADDRESS, 3000, 60)],
    )
    db_apply_v3_liquidity_updates(
        str(database_path),
        CHAIN_ID,
        POOL_ADDRESS,
        [LiquidityUpdateEvent(SEED_BLOCK, 0, -120, 120, 10**18)],
    )


@pytest.fixture
def seeded_v3_database(tmp_path: Path) -> Path:
    database_path = tmp_path / "seeded_v3.sqlite3"
    _seed_pool(database_path, _seed_exchange(database_path))
    return database_path


@pytest.fixture
def empty_v3_database(tmp_path: Path) -> Path:
    database_path = tmp_path / "empty_v3.sqlite3"
    _seed_exchange(database_path)
    return database_path


@pytest.fixture
def base_snapshot_from_database(seeded_v3_database: Path) -> Iterator[UniswapV3LiquiditySnapshot]:
    source = DatabaseSnapshot(chain_id=CHAIN_ID, database_path=seeded_v3_database)
    try:
        yield UniswapV3LiquiditySnapshot(source=source)
    finally:
        source.close()


def test_fetch_pool_from_database_snapshot(
    base_snapshot_from_database: UniswapV3LiquiditySnapshot,
):
    assert base_snapshot_from_database.tick_bitmap(POOL_ADDRESS) is not None
    assert base_snapshot_from_database.tick_data(POOL_ADDRESS) is not None
    assert POOL_ADDRESS in base_snapshot_from_database.pools


def test_liquidity_map_is_none_for_missing_pools(empty_v3_database: Path):
    source = DatabaseSnapshot(chain_id=CHAIN_ID, database_path=empty_v3_database)
    try:
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        assert snapshot.tick_bitmap(ZERO_ADDRESS) is None
        assert snapshot.tick_data(ZERO_ADDRESS) is None
    finally:
        source.close()
