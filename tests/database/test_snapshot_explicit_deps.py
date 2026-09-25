"""Tests for path-explicit Rust-backed database snapshots."""

import pathlib

import pytest

from degenbot.database.models.base import ExchangeTable
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot


def _create_database_with_exchange(db_path: pathlib.Path) -> None:
    create_new_sqlite_database(db_path)
    scoped = get_scoped_sqlite_session(db_path)
    try:
        with scoped() as session:
            session.add(
                ExchangeTable(
                    chain_id=1,
                    name="uniswap_v3",
                    last_update_block=18_000_000,
                    active=True,
                    factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
                )
            )
            session.commit()
    finally:
        scoped.remove()
        scoped.get_bind().dispose()


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
