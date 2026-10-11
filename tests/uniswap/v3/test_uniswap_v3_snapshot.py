"""V3 liquidity-snapshot read-surface tests over the database source.

The file- and directory-backed JSON snapshot sources were removed: they were
Python-only, with no Rust counterpart (ADR-005 feature parity). The facade's
drain-on-read / merge / event semantics are pinned hermetically by
``tests/uniswap/test_snapshot_facade_semantics.py``; what remains here is the
database source wiring (which routes through the Rust ``degenbot-db`` core
crate) and its read surface.
"""

from collections.abc import Iterator

import pytest

from degenbot.config import resolve_database_path
from degenbot.constants import ZERO_ADDRESS
from degenbot.fork import AnvilFork
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot, UniswapV3LiquiditySnapshot


@pytest.fixture
def base_snapshot_from_database(
    fork_base_full: AnvilFork,
) -> Iterator[UniswapV3LiquiditySnapshot]:
    source = DatabaseSnapshot(
        chain_id=8453,
        database_path=resolve_database_path(),
    )
    try:
        yield UniswapV3LiquiditySnapshot(source=source)
    finally:
        source.close()


@pytest.mark.base
def test_fetch_pool_from_database_snapshot(
    base_snapshot_from_database: UniswapV3LiquiditySnapshot,
    fork_base_full: AnvilFork,
):

    # TODO: improve test by constructing standalone database and testing against it
    # TODO: make sure that test database is upgraded with alembic

    for pool in [
        "0xe13514AaCc27a3dFd2ae0db6aDA4eF7658c1E435",
    ]:
        assert base_snapshot_from_database.tick_bitmap(pool) is not None
        assert base_snapshot_from_database.tick_data(pool) is not None
        assert pool in base_snapshot_from_database.pools


@pytest.mark.base
def test_liquidity_map_is_none_for_missing_pools(
    base_snapshot_from_database: UniswapV3LiquiditySnapshot,
):
    assert base_snapshot_from_database.tick_bitmap(ZERO_ADDRESS) is None
    assert base_snapshot_from_database.tick_data(ZERO_ADDRESS) is None
