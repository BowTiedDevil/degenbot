"""Tests for :mod:`degenbot.database.operations`.

Covers the concurrency PRAGMAs that ``get_scoped_sqlite_session`` must set on
every pooled connection: ``journal_mode=WAL``, ``busy_timeout=5000``,
``synchronous=NORMAL``.
"""

import pathlib

from sqlalchemy.orm import scoped_session

from degenbot.database.operations import get_scoped_sqlite_session


def _pragmas(session_factory) -> dict[str, str]:
    """Read the per-connection PRAGMAs from a fresh session connection."""
    with session_factory() as session:
        conn = session.connection().connection.driver_connection
        journal = conn.execute("PRAGMA journal_mode;").fetchone()[0]
        busy = conn.execute("PRAGMA busy_timeout;").fetchone()[0]
        sync = conn.execute("PRAGMA synchronous;").fetchone()[0]
    return {"journal": journal, "busy": busy, "sync": sync}


def _teardown(session_factory: scoped_session) -> None:
    """Release the scoped session and dispose the bound engine to avoid leaving
    an unclosed sqlite3 connection that raises a ResourceWarning during GC
    (which the full suite treats as an error under unraisable-exception
    checking)."""
    engine = session_factory.get_bind()
    session_factory.remove()
    engine.dispose()


class TestScopedSqliteSessionPragmas:
    def test_pragmas_set_on_opened_connection(self, tmp_path: pathlib.Path) -> None:
        """A normally-opened session must observe WAL + busy_timeout + synchronous=NORMAL."""
        db_path = tmp_path / "test.db"
        session_factory = get_scoped_sqlite_session(db_path)

        pragmas = _pragmas(session_factory)

        assert pragmas["journal"].lower() == "wal"
        assert pragmas["busy"] == 5000
        # synchronous=NORMAL reports as integer 1
        assert pragmas["sync"] == 1

        _teardown(session_factory)

    def test_pragmas_survive_pooling(self, tmp_path: pathlib.Path) -> None:
        """Every pooled connection must re-assert the per-connection PRAGMAs."""
        db_path = tmp_path / "test.db"
        session_factory = get_scoped_sqlite_session(db_path)

        first = _pragmas(session_factory)
        # Release the connection back to the pool, then open another session —
        # SQLAlchemy may hand back the same pooled connection or a fresh one;
        # either way the PRAGMAs must hold.
        second = _pragmas(session_factory)
        third = _pragmas(session_factory)

        for label, result in [("first", first), ("second", second), ("third", third)]:
            assert result["journal"].lower() == "wal", f"{label} lost journal_mode"
            assert result["busy"] == 5000, f"{label} lost busy_timeout"
            assert result["sync"] == 1, f"{label} lost synchronous"

        _teardown(session_factory)
