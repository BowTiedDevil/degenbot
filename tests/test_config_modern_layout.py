"""The Python driver owns no database bootstrap (ADR-052 / ADR-062).

The operator file's ``database.path`` resolves through the Rust cascade, and
the Rust core owns creating the database at open. These tests pin the driver's
side of that boundary: resolving a path writes nothing, the driver exposes no
creation helper, and the core's ensure-at-open is what materializes the file
and its Rust schema. They run against pinned tmp paths, so the suite never
touches an operator's state home.
"""

from __future__ import annotations

import sqlite3
from typing import TYPE_CHECKING

from degenbot import config as config_module
from degenbot.config import resolve_database_path
from degenbot.db import db_upgrade_database

if TYPE_CHECKING:
    from pathlib import Path

RUST_STAMP_TABLE = "_degenbot_db_schema_version"


def test_the_python_config_surface_exposes_no_database_bootstrap() -> None:
    """Database creation moved to the core; the driver imports no starter."""
    assert not hasattr(config_module, "db_create_new_database")


def test_resolving_a_database_path_writes_nothing(tmp_path: Path) -> None:
    """Resolution is a pure verdict: no parent directory, no file."""
    target = tmp_path / "state" / "degenbot" / "db" / "degenbot.db"

    resolved = resolve_database_path(str(target))

    assert resolved == str(target)
    assert not target.exists()
    assert not target.parent.exists()


def test_resolving_an_in_memory_database_creates_nothing(tmp_path: Path) -> None:
    """``:memory:`` has no parent and no file; resolution leaves it alone."""
    assert resolve_database_path(":memory:") == ":memory:"
    assert list(tmp_path.iterdir()) == []


def test_the_core_creates_the_database_with_its_schema(tmp_path: Path) -> None:
    """The core's ensure-at-open materializes the file and the Rust schema."""
    db_path = tmp_path / "degenbot.db"

    db_upgrade_database(str(db_path))

    assert db_path.is_file()
    with sqlite3.connect(db_path) as connection:
        tables = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            )
        }
    assert {"erc20_tokens", "exchanges", "pools"} <= tables
    assert RUST_STAMP_TABLE in tables
    assert "alembic_version" not in tables


def test_neither_resolution_nor_the_core_writes_an_operator_file(
    tmp_path: Path,
) -> None:
    """The operator file belongs to the typed Rust loader, never to a boot.

    The pre-0.6 bootstrap saved a config dump here, which wrote the retired
    ``default_chain_id`` key the typed loader refuses at boot.
    """
    db_path = tmp_path / "degenbot.db"
    config_module.resolve_database_path(str(db_path))
    db_upgrade_database(str(db_path))

    assert list(tmp_path.glob("**/*.toml")) == []
