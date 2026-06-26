"""Tests for :class:`DatabaseSessionManager` engine lifecycle.

``dispose()`` must release the underlying SQLAlchemy ``Engine`` bound to the
scoped session, so closing a Bot does not leave an unclosed ``sqlite3``
connection (``ResourceWarning: unclosed database``). Idempotent.
"""

import pathlib
from unittest.mock import patch

from sqlalchemy.orm import scoped_session, sessionmaker

from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager


def _make_manager(*, memory: bool = True) -> DatabaseSessionManager:
    path = pathlib.Path(":memory:") if memory else pathlib.Path("test.db")
    return DatabaseSessionManager(get_scoped_sqlite_session(path))


class TestDatabaseSessionManagerDispose:
    def test_dispose_calls_engine_dispose(self) -> None:
        manager = _make_manager()
        engine = manager._engine
        assert engine is not None

        with patch.object(engine, "dispose", wraps=engine.dispose) as spy:
            manager.dispose()
            spy.assert_called_once_with()

    def test_dispose_is_idempotent(self) -> None:
        manager = _make_manager()
        manager.dispose()
        manager.dispose()  # second call must be a no-op, not raise

    def test_dispose_after_explicit_session_remove(self) -> None:
        """The documented close sequence (remove then dispose) must not raise."""
        manager = _make_manager()
        manager.remove()
        manager.dispose()

    def test_dispose_on_manager_without_bind_is_noop(self) -> None:
        """A manager whose scoped session has no engine bind must not raise."""
        factory = sessionmaker()  # no bind
        ss = scoped_session(factory)
        manager = DatabaseSessionManager(ss)
        manager.dispose()  # must not raise
