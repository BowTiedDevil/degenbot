"""Behavior tests for Rust-backed SQLite file operations."""

from __future__ import annotations

import filecmp
import pathlib

import pytest

from degenbot.db import (
    db_backup_database,
    db_compact_database,
    db_create_new_database,
    db_fetch_exchange_by_name,
    db_heal_database,
    db_schema_version,
    db_upgrade_database,
    db_upsert_exchange,
)
from tests.helpers.database import sqlite_connection

RUST_STAMP_TABLE = "_degenbot_db_schema_version"
CURRENT_RUST_SCHEMA_VERSION = db_schema_version()
assert CURRENT_RUST_SCHEMA_VERSION >= 2


def _tables(db_path: pathlib.Path) -> set[str]:
    with sqlite_connection(db_path) as connection:
        return {
            row[0]
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }


def _journal_mode(db_path: pathlib.Path) -> str:
    with sqlite_connection(db_path) as connection:
        return str(connection.execute("PRAGMA journal_mode").fetchone()[0])


def _rust_stamp(db_path: pathlib.Path) -> int:
    with sqlite_connection(db_path) as connection:
        return int(
            connection.execute(f"SELECT schema_version FROM {RUST_STAMP_TABLE}").fetchone()[0]
        )


def _mark_legacy(db_path: pathlib.Path) -> None:
    with sqlite_connection(db_path) as connection:
        connection.execute(f"DROP TABLE {RUST_STAMP_TABLE}")
        connection.execute("CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL)")
        connection.execute("INSERT INTO alembic_version VALUES ('e0aaad8ad486')")


def test_create_new_database_is_wal_and_rust_stamped(tmp_path: pathlib.Path):
    db_path = tmp_path / "fresh.db"
    db_create_new_database(str(db_path))

    assert db_path.exists()
    assert _journal_mode(db_path) == "wal"
    assert RUST_STAMP_TABLE in _tables(db_path)
    assert "alembic_version" not in _tables(db_path)
    assert _rust_stamp(db_path) == CURRENT_RUST_SCHEMA_VERSION


def test_backup_is_byte_stable_and_reopens(tmp_path: pathlib.Path):
    src = tmp_path / "src.db"
    db_create_new_database(str(src))

    first = tmp_path / "first.bak"
    second = tmp_path / "second.bak"
    db_backup_database(str(src), str(first))
    db_backup_database(str(src), str(second))

    assert filecmp.cmp(first, second, shallow=False)
    assert RUST_STAMP_TABLE in _tables(first)
    assert _rust_stamp(first) == CURRENT_RUST_SCHEMA_VERSION


def test_compact_is_idempotent_and_preserves_stamp(tmp_path: pathlib.Path):
    db_path = tmp_path / "compact.db"
    db_create_new_database(str(db_path))

    db_compact_database(str(db_path))
    db_compact_database(str(db_path))

    assert _rust_stamp(db_path) == CURRENT_RUST_SCHEMA_VERSION
    assert _journal_mode(db_path) == "wal"


def test_upgrade_on_current_db_is_noop(tmp_path: pathlib.Path):
    db_path = tmp_path / "current.db"
    db_create_new_database(str(db_path))
    assert db_upgrade_database(str(db_path)) == "already_current"
    assert _rust_stamp(db_path) == CURRENT_RUST_SCHEMA_VERSION


def test_upgrade_on_empty_file_brings_up_to_current(tmp_path: pathlib.Path):
    db_path = tmp_path / "empty.db"
    db_path.write_bytes(b"")
    assert db_upgrade_database(str(db_path)) == "created_fresh"
    assert RUST_STAMP_TABLE in _tables(db_path)
    assert _rust_stamp(db_path) == CURRENT_RUST_SCHEMA_VERSION


def test_upgrade_on_legacy_marker_heals(tmp_path: pathlib.Path):
    db_path = tmp_path / "legacy.db"
    db_create_new_database(str(db_path))
    _mark_legacy(db_path)

    assert db_upgrade_database(str(db_path)) == "healed_legacy"
    assert RUST_STAMP_TABLE in _tables(db_path)
    assert "alembic_version" not in _tables(db_path)
    assert _rust_stamp(db_path) == CURRENT_RUST_SCHEMA_VERSION


def test_heal_round_trips_through_seam(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("DEGENBOT_DB_AUTO_HEAL", "0")
    db_path = tmp_path / "heal.db"
    db_create_new_database(str(db_path))
    _mark_legacy(db_path)

    row = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=1,
        name="uniswap_v2",
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        deployer=None,
    )
    report = db_heal_database(str(db_path))

    assert report["old_state"] == "legacy_alembic"
    assert report["new_state"] == "rust_owned"
    assert report["rows_copied"]["exchanges"] == 1
    assert report["warnings"] == []
    backup = pathlib.Path(report["bak_path"])
    assert "alembic_version" in _tables(backup)
    assert "alembic_version" not in _tables(db_path)

    fetched = db_fetch_exchange_by_name(str(db_path), 1, "uniswap_v2")
    assert fetched is not None
    assert fetched.id == row.id


def test_console_inspect_renders_legacy_state(tmp_path: pathlib.Path, capfd):
    import degenbot._ffi as ffi

    db_path = tmp_path / "legacy_cli.db"
    db_create_new_database(str(db_path))
    _mark_legacy(db_path)

    code = ffi.cli_main(["--database", str(db_path), "database", "inspect"])
    captured = capfd.readouterr()
    assert code == 0
    assert "legacy_alembic" in captured.out + captured.err
    assert "Traceback" not in captured.err
