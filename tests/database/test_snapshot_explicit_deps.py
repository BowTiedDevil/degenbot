"""Tests for path-explicit Rust-backed database snapshots."""

import pathlib

import pytest

from degenbot.db import (
    db_create_new_database,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
)
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot


def _create_database_with_exchange(db_path: pathlib.Path) -> None:
    db_create_new_database(str(db_path))
    exchange = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=1,
        name="uniswap_v3",
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        deployer=None,
    )
    db_set_exchange_last_update_block(
        database_path=str(db_path),
        chain_id=1,
        exchange_id=exchange.id,
        block=18_000_000,
    )


class TestV3DatabaseSnapshotExplicitPath:
    def test_constructor_retains_explicit_path(self, tmp_path: pathlib.Path) -> None:
        db_path = tmp_path / "test.db"

        snapshot = V3DatabaseSnapshot(chain_id=1, database_path=db_path)

        assert snapshot.database_path == db_path
        assert snapshot.chain_id == 1
        assert not hasattr(snapshot, "session")

    def test_context_manager_releases_rust_handle(self, tmp_path: pathlib.Path) -> None:
        db_path = tmp_path / "test.db"
        _create_database_with_exchange(db_path)

        snapshot = V3DatabaseSnapshot(chain_id=1, database_path=db_path)
        with snapshot as entered:
            assert entered is snapshot
            assert entered.get_newest_block() == 18_000_000

        with pytest.raises(RuntimeError, match="closed"):
            snapshot.get_newest_block()
        snapshot.close()

    def test_get_newest_block_reads_via_rust_seam(self, tmp_path: pathlib.Path) -> None:
        db_path = tmp_path / "test.db"
        _create_database_with_exchange(db_path)

        with V3DatabaseSnapshot(chain_id=1, database_path=db_path) as snapshot:
            assert snapshot.get_newest_block() == 18_000_000


class TestV4DatabaseSnapshotExplicitPath:
    def test_constructor_retains_explicit_path(self, tmp_path: pathlib.Path) -> None:
        db_path = tmp_path / "test.db"

        snapshot = V4DatabaseSnapshot(chain_id=1, database_path=db_path)

        assert snapshot.database_path == db_path
        assert snapshot.chain_id == 1
        assert not hasattr(snapshot, "session")
