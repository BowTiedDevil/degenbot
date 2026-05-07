"""Tests for snapshot explicit-dependency injection (Phase 8).

Verifies that DatabaseSnapshot and fetch_new_events/fetch_new_events_async
accept explicit dependencies instead of importing module-level singletons.
"""

import pathlib
from unittest.mock import MagicMock, AsyncMock

import pytest

from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.uniswap.v3_snapshot import (
    DatabaseSnapshot as V3DatabaseSnapshot,
)
from degenbot.uniswap.v4_snapshot import (
    DatabaseSnapshot as V4DatabaseSnapshot,
)


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
        """get_newest_block uses self.session() instead of module-level db_session."""
        from sqlalchemy import create_engine
        from sqlalchemy.orm import scoped_session, sessionmaker

        engine = create_engine("sqlite:///:memory:")
        from degenbot.database.models.base import Base

        Base.metadata.create_all(engine)

        SessionLocal = sessionmaker(bind=engine)
        session = SessionLocal()

        # Insert a test exchange with a last_update_block
        from degenbot.database.models.base import ExchangeTable

        exchange = ExchangeTable(
            chain_id=1,
            name="uniswap_v3",
            last_update_block=18_000_000,
            active=True,
            factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        )
        session.add(exchange)
        session.commit()

        scoped = scoped_session(lambda: SessionLocal())
        db = DatabaseSessionManager(scoped)

        snapshot = V3DatabaseSnapshot(
            chain_id=1,
            db=db,
            database_path=tmp_path / "test.db",
        )

        result = snapshot.get_newest_block()
        assert result == 18_000_000


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


class TestV3FetchNewEventsExplicitProvider:
    """V3 fetch_new_events accepts an explicit provider parameter."""

    def test_fetch_new_events_accepts_provider_kwarg(self) -> None:
        """fetch_new_events should accept provider= instead of using connection_manager."""
        from degenbot.uniswap.v3_snapshot import (
            MonolithicJsonFileSnapshot,
            UniswapV3LiquiditySnapshot,
        )

        snapshot = UniswapV3LiquiditySnapshot(
            source=MonolithicJsonFileSnapshot(
                "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
            ),
        )

        # Verify the method signature accepts provider=
        import inspect

        sig = inspect.signature(snapshot.fetch_new_events)
        assert "provider" in sig.parameters


class TestV4FetchNewEventsExplicitProvider:
    """V4 fetch_new_events accepts an explicit provider parameter."""

    def test_fetch_new_events_accepts_provider_kwarg(self) -> None:
        """fetch_new_events should accept provider= instead of using connection_manager."""
        from degenbot.uniswap.v4_snapshot import (
            MonolithicJsonFileSnapshot,
            UniswapV4LiquiditySnapshot,
        )

        snapshot = UniswapV4LiquiditySnapshot(
            source=MonolithicJsonFileSnapshot(
                "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
            ),
        )

        import inspect

        sig = inspect.signature(snapshot.fetch_new_events)
        assert "provider" in sig.parameters

    def test_fetch_new_events_async_accepts_w3_kwarg(self) -> None:
        """fetch_new_events_async should accept w3= instead of using async_connection_manager."""
        from degenbot.uniswap.v4_snapshot import (
            MonolithicJsonFileSnapshot,
            UniswapV4LiquiditySnapshot,
        )

        snapshot = UniswapV4LiquiditySnapshot(
            source=MonolithicJsonFileSnapshot(
                "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
            ),
        )

        import inspect

        sig = inspect.signature(snapshot.fetch_new_events_async)
        assert "w3" in sig.parameters
