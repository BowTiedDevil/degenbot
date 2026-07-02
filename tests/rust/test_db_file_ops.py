"""§4.2 parity for the Rust-backed SQLite file operations (ergo OP23QV).

The four file ops now delegate to the Rust core (`degenbot_rs.db_*` over
`degenbot-db::ops`). These tests drive the Python wrappers in
`degenbot.database.operations` against a temp DB and assert the file-level
invariants the acceptance criteria pin:

- `create_new_sqlite_database` → WAL journal mode + Alembic `head` stamp.
- `backup_sqlite_database` → a byte-stable backup that reopens as a valid DB
  and whose repeated backups are byte-identical.
- `compact_sqlite_database` → `VACUUM` is idempotent / non-destructive.
- `upgrade_existing_sqlite_database` → no-op on an already-head DB; brings an
  empty file up to head.

The Python path is the oracle for cross-implementation byte-shape parity
(§4.3 retirement is a follow-up, not in scope here).
"""

from __future__ import annotations

import filecmp
import pathlib

import pytest
from alembic.script import ScriptDirectory
from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.database.operations import (
    backup_sqlite_database,
    compact_sqlite_database,
    create_new_sqlite_database,
    get_alembic_config,
    upgrade_existing_sqlite_database,
)
from degenbot.degenbot_rs import (
    db_backup_database,
    db_create_new_database,
    db_upgrade_database,
)


def _alembic_head_expected() -> str:
    """The current Alembic head revision (the Python migration toolchain oracle)."""
    return ScriptDirectory.from_config(
        config=get_alembic_config(database_path=pathlib.Path("nonexistent.db")),
    ).get_current_head()


def _alembic_head(db_path: pathlib.Path) -> str:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            return conn.execute(
                text("SELECT version_num FROM alembic_version")
            ).scalar()
    finally:
        engine.dispose()


def _journal_mode(db_path: pathlib.Path) -> str:
    engine = create_engine(f"sqlite:///{db_path}")
    try:
        with engine.connect() as conn:
            return conn.execute(text("PRAGMA journal_mode;")).scalar()
    finally:
        engine.dispose()


def test_create_new_database_is_wal_and_stamped_at_head(tmp_path: pathlib.Path):
    db_path = tmp_path / "fresh.db"
    create_new_sqlite_database(db_path)

    assert db_path.exists()
    assert _journal_mode(db_path) == "wal"
    assert _alembic_head(db_path) == _alembic_head_expected()


def test_rust_and_python_create_produce_equivalent_files(tmp_path: pathlib.Path):
    """A Rust-created DB and a Python-oracle-created DB agree on WalMode + head."""
    rust_path = tmp_path / "rust.db"
    db_create_new_database(str(rust_path))
    py_path = tmp_path / "py.db"
    create_new_sqlite_database(py_path)

    assert _journal_mode(rust_path) == _journal_mode(py_path) == "wal"
    assert _alembic_head(rust_path) == _alembic_head(py_path) == _alembic_head_expected()


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

    # the backup reopens at the same Alembic head
    assert _alembic_head(bak1) == _alembic_head_expected()


def test_backup_raises_backup_exists_guard(tmp_path: pathlib.Path, monkeypatch):
    src = tmp_path / "src.db"
    create_new_sqlite_database(src)
    bak = src.with_suffix(".db.bak")
    bak.write_bytes(b"existing")

    from degenbot.exceptions.infrastructure import BackupExists

    with pytest.raises(BackupExists):
        backup_sqlite_database(session=_session_for(src), skip_confirmation=False)


def test_compact_is_idempotent_and_preserves_head(tmp_path: pathlib.Path):
    db_path = tmp_path / "compact.db"
    create_new_sqlite_database(db_path)
    head_before = _alembic_head(db_path)

    compact_sqlite_database(db_path)
    compact_sqlite_database(db_path)

    assert _alembic_head(db_path) == head_before == _alembic_head_expected()
    assert _journal_mode(db_path) == "wal"


def test_upgrade_on_head_db_is_noop(tmp_path: pathlib.Path):
    db_path = tmp_path / "head.db"
    create_new_sqlite_database(db_path)
    outcome = upgrade_existing_sqlite_database(db_path)
    assert outcome == "already_at_head"
    assert _alembic_head(db_path) == _alembic_head_expected()


def test_upgrade_on_empty_file_brings_up_to_head(tmp_path: pathlib.Path):
    db_path = tmp_path / "empty.db"
    db_path.write_bytes(b"")
    outcome = db_upgrade_database(str(db_path))
    assert outcome == "created_fresh"
    assert _alembic_head(db_path) == _alembic_head_expected()


def _session_for(db_path: pathlib.Path) -> Session:
    """A minimal SQLAlchemy Session bound to `db_path` (for the backup shell)."""
    engine = create_engine(f"sqlite:///{db_path}")
    return Session(bind=engine)
