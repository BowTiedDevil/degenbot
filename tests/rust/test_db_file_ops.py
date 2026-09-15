"""§4.2 parity for the Rust-backed SQLite file operations.

The four file ops delegate to the Rust core (``degenbot._ffi.db_*`` over
``degenbot-db::ops``). These tests drive the Python wrappers in
``degenbot.database.operations`` against a temp DB and assert the file-level
invariants the acceptance criteria pin:

- ``create_new_sqlite_database`` → WAL journal mode + the Rust schema stamp.
- ``backup_sqlite_database`` → a byte-stable backup that reopens as a valid DB
  and whose repeated backups are byte-identical.
- ``compact_sqlite_database`` → ``VACUUM`` is idempotent / non-destructive.
- ``db_upgrade_database`` → no-op on a Rust-owned DB; brings an empty file up
  to the current Rust schema; heals a legacy ``alembic_version``-marked DB.

The schema is Rust-owned and upgrades itself at open (ADR-052); Python is a
thin driver shell.
"""

from __future__ import annotations

import filecmp
import pathlib

import pytest
from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.database.operations import (
    backup_sqlite_database,
    compact_sqlite_database,
    create_new_sqlite_database,
)
from degenbot.db import (
    db_backup_database,
    db_create_new_database,
    db_fetch_exchange_by_name,
    db_heal_database,
    db_upgrade_database,
    db_upsert_exchange,
)

RUST_STAMP_TABLE = "_degenbot_db_schema_version"


def _tables(db_path: pathlib.Path) -> set[str]:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            return {
                r[0]
                for r in conn.execute(
                    text("SELECT name FROM sqlite_master WHERE type='table'")
                )
            }
    finally:
        engine.dispose()


def _journal_mode(db_path: pathlib.Path) -> str:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            return conn.execute(text("PRAGMA journal_mode;")).scalar()
    finally:
        engine.dispose()


def _rust_stamp(db_path: pathlib.Path) -> int:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            return conn.execute(
                text(f"SELECT schema_version FROM {RUST_STAMP_TABLE}")
            ).scalar()
    finally:
        engine.dispose()


def _mark_legacy(db_path: pathlib.Path) -> None:
    """Flip a Rust-owned DB to the legacy ``alembic_version``-marked shape."""
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.begin() as conn:
            conn.execute(text(f"DROP TABLE {RUST_STAMP_TABLE};"))
            conn.execute(
                text("CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);")
            )
            conn.execute(
                text("INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');")
            )
    finally:
        engine.dispose()


def _index_exists(db_path: pathlib.Path, index_name: str) -> bool:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            row = conn.execute(
                text("SELECT name FROM sqlite_master WHERE type='index' AND name=:n;"),
                {"n": index_name},
            ).scalar()
            return row is not None
    finally:
        engine.dispose()


def _session_for(db_path: pathlib.Path) -> Session:
    """A minimal SQLAlchemy Session bound to `db_path` (for the backup shell)."""
    engine = create_engine(f"sqlite:///{db_path}")
    return Session(bind=engine)


def test_create_new_database_is_wal_and_rust_stamped(tmp_path: pathlib.Path):
    db_path = tmp_path / "fresh.db"
    create_new_sqlite_database(db_path)

    assert db_path.exists()
    assert _journal_mode(db_path) == "wal"
    tables = _tables(db_path)
    assert RUST_STAMP_TABLE in tables
    assert "alembic_version" not in tables
    assert _rust_stamp(db_path) == 1


def test_rust_and_python_create_produce_equivalent_files(tmp_path: pathlib.Path):
    """A Rust-created DB and a Python-driver-created DB agree on WAL + stamp."""
    rust_path = tmp_path / "rust.db"
    db_create_new_database(str(rust_path))
    py_path = tmp_path / "py.db"
    create_new_sqlite_database(py_path)

    assert _journal_mode(rust_path) == _journal_mode(py_path) == "wal"
    assert _rust_stamp(rust_path) == _rust_stamp(py_path) == 1
    assert _tables(rust_path) == _tables(py_path)


def test_backup_is_byte_stable_and_reopens(tmp_path: pathlib.Path):
    src = tmp_path / "src.db"
    create_new_sqlite_database(src)

    bak1 = tmp_path / "src.db.bak"
    backup_sqlite_database(session=_session_for(src), skip_confirmation=True)

    assert bak1.exists()
    # byte-stable: a second backup of the unchanged source is byte-identical
    bak2 = tmp_path / "second.bak"
    db_backup_database(str(src), str(bak2))
    assert filecmp.cmp(bak1, bak2, shallow=False)

    # the backup reopens with the same Rust stamp
    assert RUST_STAMP_TABLE in _tables(bak1)
    assert _rust_stamp(bak1) == 1


def test_backup_raises_backup_exists_guard(tmp_path: pathlib.Path, monkeypatch):
    src = tmp_path / "src.db"
    create_new_sqlite_database(src)
    bak = src.with_suffix(".db.bak")
    bak.write_bytes(b"existing")

    from degenbot.exceptions.infrastructure import BackupExists

    with pytest.raises(BackupExists):
        backup_sqlite_database(session=_session_for(src), skip_confirmation=False)


def test_compact_is_idempotent_and_preserves_stamp(tmp_path: pathlib.Path):
    db_path = tmp_path / "compact.db"
    create_new_sqlite_database(db_path)

    compact_sqlite_database(db_path)
    compact_sqlite_database(db_path)

    assert _rust_stamp(db_path) == 1
    assert _journal_mode(db_path) == "wal"


def test_upgrade_on_current_db_is_noop(tmp_path: pathlib.Path):
    db_path = tmp_path / "current.db"
    create_new_sqlite_database(db_path)
    assert db_upgrade_database(str(db_path)) == "already_current"
    assert _rust_stamp(db_path) == 1


def test_upgrade_on_empty_file_brings_up_to_current(tmp_path: pathlib.Path):
    db_path = tmp_path / "empty.db"
    db_path.write_bytes(b"")
    assert db_upgrade_database(str(db_path)) == "created_fresh"
    assert RUST_STAMP_TABLE in _tables(db_path)
    assert _rust_stamp(db_path) == 1


def test_upgrade_on_legacy_marker_heals(tmp_path: pathlib.Path):
    db_path = tmp_path / "legacy.db"
    create_new_sqlite_database(db_path)
    _mark_legacy(db_path)
    assert "alembic_version" in _tables(db_path)

    assert db_upgrade_database(str(db_path)) == "healed_legacy"

    tables = _tables(db_path)
    assert RUST_STAMP_TABLE in tables
    assert "alembic_version" not in tables
    assert _rust_stamp(db_path) == 1


def test_heal_round_trips_through_seam(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
):
    """`db_heal_database` (ADR-011) round-trips through the PyO3 seam.

    Pin the `DEGENBOT_DB_AUTO_HEAL=0` killswitch for THIS test only: the write
    seam would otherwise auto-heal the legacy DB at open and change
    `old_state` to rust_owned before the explicit heal runs.
    """
    monkeypatch.setenv("DEGENBOT_DB_AUTO_HEAL", "0")
    db_path = tmp_path / "heal.db"
    db_path_str = str(db_path)
    db_create_new_database(db_path_str)
    _mark_legacy(db_path)

    # Write an exchange row via the Rust write seam (legacy DB, killswitch on).
    factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"  # Uniswap V2 mainnet
    row = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
        factory=factory,
        deployer=None,
    )
    assert row.id is not None

    # Heal: out-of-place dump-and-restore. Returns the HealReport as a dict.
    report = db_heal_database(db_path_str)

    assert set(report.keys()) == {
        "old_state",
        "rows_copied",
        "bak_path",
        "new_state",
        "warnings",
    }
    assert report["old_state"] == "legacy_alembic"
    assert report["new_state"] == "rust_owned"
    assert report["rows_copied"]["exchanges"] == 1
    assert report["warnings"] == []

    # The .bak exists on disk + holds the OLD (legacy-owned) data.
    bak_path = pathlib.Path(report["bak_path"])
    assert bak_path.exists()
    assert "alembic_version" in _tables(bak_path)

    # The healed (live) DB is now Rust-owned + the exchange row survives.
    tables = _tables(db_path)
    assert "alembic_version" not in tables
    assert RUST_STAMP_TABLE in tables

    fetched = db_fetch_exchange_by_name(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
    )
    assert fetched is not None
    assert fetched.id == row.id


def test_console_inspect_renders_legacy_state(tmp_path: pathlib.Path, capfd):
    """The Rust console renders a legacy-marker DB state cleanly, exit 0."""
    import degenbot._ffi as _ffi

    db_path = tmp_path / "legacy_cli.db"
    create_new_sqlite_database(db_path)
    _mark_legacy(db_path)

    code = _ffi.cli_main(["--database", str(db_path), "database", "inspect"])
    captured = capfd.readouterr()
    assert code == 0
    assert "legacy_alembic" in captured.out + captured.err
    assert "Traceback" not in captured.err
