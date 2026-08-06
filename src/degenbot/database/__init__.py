"""Database session management and Alembic configuration."""

from typing import TYPE_CHECKING

from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager

if TYPE_CHECKING:
    # ADR-013: stable home for the FFI ERC-20 DB row type so a leaf module
    # (degenbot.builders.erc20_builder) need not import degenbot._ffi directly.
    # TYPE_CHECKING-only: it is used solely in annotations (`from __future__
    # import annotations` keeps those lazy at runtime).
    from degenbot._ffi import Erc20TokenRow as Erc20TokenRow

__all__ = (
    "DatabaseSessionManager",
    "get_alembic_config",
    "get_scoped_sqlite_session",
)
