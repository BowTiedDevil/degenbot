"""Tests for snapshot explicit-dependency injection.

Verifies that DatabaseSnapshot accepts explicit dependencies
instead of importing module-level singletons.
"""

import pathlib
from unittest.mock import MagicMock

from degenbot.database.models.base import ExchangeTable
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot


class TestV3DatabaseSnapshotExplicitDeps:
    """V3 DatabaseSnapshot accepts explicit db and database_path."""

    def test_init_with_explicit_db_and_path(self, tmp_path: pathlib.Path) -> None:
        """DatabaseSnapshot no longer falls back to config/db_session globals."""
        fake_session = MagicMock()
        db = DatabaseSessionManager.__new__(DatabaseSessionManager)
        db._session = fake_session

        db_path = tmp_path / "test.db"

        snapshot = V3DatabaseSnapshot(
            chain_id=1,
            db=db,
            database_path=db_path,
        )

        assert snapshot.session is db
        assert snapshot.database_path == db_path
        assert snapshot.chain_id == 1

    def test_get_newest_block_uses_self_session(self, tmp_path: pathlib.Path) -> None:
        """get_newest_block reads via the Rust seam from the file DB.

        The snapshot is no longer SQLAlchemy-backed for reads; this constructs
        a real Alembic-stamped file DB, inserts an exchange via the ORM session,
        then asserts the Rust-backed `get_newest_block` reads it.
        """
        db_path = tmp_path / "test.db"
        create_new_sqlite_database(db_path)
        scoped = get_scoped_sqlite_session(db_path)

        with scoped() as s:
            exchange = ExchangeTable(
                chain_id=1,
                name="uniswap_v3",
                last_update_block=18_000_000,
                active=True,
                factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
            )
            s.add(exchange)
            s.commit()

        db = DatabaseSessionManager(scoped)

        snapshot = V3DatabaseSnapshot(
            chain_id=1,
            db=db,
            database_path=db_path,
        )

        assert snapshot.get_newest_block() == 18_000_000

        db.dispose()


class TestV4DatabaseSnapshotExplicitDeps:
    """V4 DatabaseSnapshot accepts explicit db and database_path."""

    def test_init_with_explicit_db_and_path(self, tmp_path: pathlib.Path) -> None:
        fake_session = MagicMock()
        db = DatabaseSessionManager.__new__(DatabaseSessionManager)
        db._session = fake_session

        db_path = tmp_path / "test.db"

        snapshot = V4DatabaseSnapshot(
            chain_id=1,
            db=db,
            database_path=db_path,
        )

        assert snapshot.session is db
        assert snapshot.database_path == db_path
        assert snapshot.chain_id == 1
