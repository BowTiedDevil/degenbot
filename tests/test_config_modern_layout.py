"""The database bootstrap the Python driver still owns (ADR-062 D9 follow-up).

Python never writes the operator file — an absent file is contractually the
schema defaults, and the typed Rust loader is the only writer — so the one
bootstrap left on this side is the database: create its parent directory and
initialize an empty file when one is not there yet. These tests pin that
bootstrap (and its in-memory exemption) against a pinned path, so the suite
never writes into an operator's state home.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot import config as config_module
from degenbot.config import _init_config

if TYPE_CHECKING:
    from pathlib import Path


@pytest.fixture
def database_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Pin the cascade's database answer into the test's tmp tree."""
    path = tmp_path / "state" / "degenbot" / "db" / "degenbot.db"
    monkeypatch.setattr(config_module, "resolve_database_path", lambda _database=None: str(path))
    return path


def test_the_bootstrap_creates_the_parent_and_the_file(
    database_path: Path,
) -> None:
    """A cold session must end up with an openable database file."""
    resolved = _init_config()

    assert resolved == str(database_path)
    assert database_path.is_file()


def test_the_bootstrap_is_idempotent(database_path: Path) -> None:
    """A second session must not re-initialize a populated database."""
    _init_config()
    database_path.write_text("sentinel", encoding="utf-8")

    _init_config()

    assert database_path.read_text(encoding="utf-8") == "sentinel"


def test_an_in_memory_database_creates_nothing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``:memory:`` has no parent and no file; the bootstrap leaves it alone."""
    monkeypatch.setattr(
        config_module, "resolve_database_path", lambda _database=None: ":memory:"
    )

    assert _init_config() == ":memory:"
    assert list(tmp_path.iterdir()) == []


def test_the_bootstrap_writes_no_operator_file(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The operator file belongs to the typed Rust layer, never to this side.

    The pre-0.6 bootstrap saved a config dump here, which wrote the retired
    ``default_chain_id`` key the typed loader refuses at boot.
    """
    monkeypatch.setattr(
        config_module,
        "resolve_database_path",
        lambda _database=None: str(tmp_path / "state" / "degenbot.db"),
    )
    created: list[str] = []
    monkeypatch.setattr(config_module, "db_create_new_database", created.append)

    _init_config()

    assert created == [str(tmp_path / "state" / "degenbot.db")]
    assert list(tmp_path.glob("**/*.toml")) == []
