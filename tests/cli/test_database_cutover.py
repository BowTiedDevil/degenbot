"""CLI tests for `degenbot database cutover` (ADR-010, ergo task FF5YGT).

The cutover command is the opt-in one-way flip from Alembic to Rust schema
ownership. It surfaces the core substrate from HTG7SL (`020dffdf`)
— `migrate::convert_alembic_to_rust_owned` + the `RustOwned` state + the
`classify_schema` pure-predicate refactor — via two PyO3 seams
(`db_inspect_schema_state` read-only dry-run, `db_convert_alembic_to_rust_owned`)
and a Click command.

Cases:
- `--dry-run` on an AlembicCurrent DB → prints the state + "would cutover",
  writes nothing (still AlembicCurrent on re-inspect).
- `--force` cutover on AlembicCurrent → "converted"; re-inspect → "rust_owned".
- `--force` on an already-RustOwned DB → "already_rust_owned" no-op.
- `--dry-run` on a stale DB → "alembic_stale" + the upgrade hint, exit 0.
- `--force` on a stale DB → friendly stale message + non-zero exit (no traceback).
- `--force` on a foreign (unrecognized) file → "unrecognized" message + non-zero.
"""

from __future__ import annotations

import sqlite3
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest
from click.testing import CliRunner

from degenbot.cli.database import database_cutover
from degenbot.database.operations import create_new_sqlite_database
from degenbot.degenbot_rs import db_inspect_schema_state

if TYPE_CHECKING:
    import pathlib


class _StubBot:
    """The cutover command touches only `bot.config.database.path`."""

    def __init__(self, path: str) -> None:
        self.config = SimpleNamespace(database=SimpleNamespace(path=path))


@pytest.fixture
def fresh_alembic_db(tmp_path: pathlib.Path) -> _StubBot:
    """A freshly-created DB stamped at the Alembic head (AlembicCurrent)."""
    db_path = tmp_path / "alembic.db"
    create_new_sqlite_database(db_path)
    db_path.chmod(0o644)
    return _StubBot(str(db_path))


def _run(runner: CliRunner, args: list[str], bot: _StubBot):
    return runner.invoke(database_cutover, args, obj=bot, catch_exceptions=False)


# ── dry-run ───────────────────────────────────────────────────────────────


def test_dry_run_on_alembic_current_writes_nothing(fresh_alembic_db: _StubBot) -> None:
    runner = CliRunner()
    before = db_inspect_schema_state(fresh_alembic_db.config.database.path)
    assert before == "alembic_current"

    result = _run(runner, ["--dry-run"], fresh_alembic_db)
    assert result.exit_code == 0, result.output
    assert "alembic_current" in result.output
    assert "would cutover" in result.output.lower()

    # wrote nothing — still AlembicCurrent, alembic_version still present.
    after = db_inspect_schema_state(fresh_alembic_db.config.database.path)
    assert after == "alembic_current"
    conn = sqlite3.connect(fresh_alembic_db.config.database.path)
    try:
        assert conn.execute(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='alembic_version'",
        ).fetchone()[0] == 1
    finally:
        conn.close()


# ── force cutover ─────────────────────────────────────────────────────────


def test_force_cutover_on_alembic_current_reports_converted(fresh_alembic_db: _StubBot) -> None:
    runner = CliRunner()
    result = _run(runner, ["--force"], fresh_alembic_db)
    assert result.exit_code == 0, result.output
    assert "Cut over" in result.output
    assert "alembic_version" in result.output  # mention the dropped table

    # now Rust-owned; alembic_version gone.
    assert db_inspect_schema_state(fresh_alembic_db.config.database.path) == "rust_owned"
    conn = sqlite3.connect(fresh_alembic_db.config.database.path)
    try:
        assert conn.execute(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='alembic_version'",
        ).fetchone()[0] == 0
    finally:
        conn.close()


def test_force_cutover_on_already_rust_owned_is_noop(fresh_alembic_db: _StubBot) -> None:
    runner = CliRunner()
    _run(runner, ["--force"], fresh_alembic_db)  # first cutover
    assert db_inspect_schema_state(fresh_alembic_db.config.database.path) == "rust_owned"

    result = _run(runner, ["--force"], fresh_alembic_db)
    assert result.exit_code == 0, result.output
    assert "already Rust-owned" in result.output
    assert db_inspect_schema_state(fresh_alembic_db.config.database.path) == "rust_owned"


# ── stale DB ──────────────────────────────────────────────────────────────


@pytest.fixture
def stale_alembic_db(tmp_path: pathlib.Path) -> _StubBot:
    """A DB stamped at a non-head Alembic revision (AlembicStale)."""
    db_path = tmp_path / "stale.db"
    conn = sqlite3.connect(str(db_path))
    try:
        conn.execute(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL)",
        )
        conn.execute("INSERT INTO alembic_version (version_num) VALUES ('deadbeefdead')")
        conn.commit()
    finally:
        conn.close()
    db_path.chmod(0o644)
    return _StubBot(str(db_path))


def test_dry_run_on_stale_reports_stale_with_upgrade_hint(stale_alembic_db: _StubBot) -> None:
    runner = CliRunner()
    result = _run(runner, ["--dry-run"], stale_alembic_db)
    assert result.exit_code == 0, result.output
    assert "alembic_stale" in result.output
    assert "stale" in result.output.lower()
    assert "upgrade" in result.output.lower()


def test_force_cutover_on_stale_exits_nonzero_no_traceback(
    stale_alembic_db: _StubBot,
) -> None:
    runner = CliRunner()
    result = _run(runner, ["--force"], stale_alembic_db)
    assert result.exit_code != 0
    assert result.exception is None or isinstance(result.exception, SystemExit)
    assert "stale" in result.output.lower()
    assert "upgrade" in result.output.lower()
    # nothing changed — still stale.
    assert db_inspect_schema_state(stale_alembic_db.config.database.path) == "alembic_stale"


# ── unrecognized (foreign file) ───────────────────────────────────────────


def test_dry_run_on_unrecognized_reports_unrecognized(tmp_path: pathlib.Path) -> None:
    db_path = tmp_path / "foreign.db"
    conn = sqlite3.connect(str(db_path))
    try:
        conn.execute("CREATE TABLE other_table (x INTEGER)")
        conn.commit()
    finally:
        conn.close()
    db_path.chmod(0o644)
    bot = _StubBot(str(db_path))

    runner = CliRunner()
    result = _run(runner, ["--dry-run"], bot)
    assert result.exit_code == 0, result.output
    assert "unrecognized" in result.output


def test_force_cutover_on_unrecognized_exits_nonzero(tmp_path: pathlib.Path) -> None:
    db_path = tmp_path / "foreign2.db"
    conn = sqlite3.connect(str(db_path))
    try:
        conn.execute("CREATE TABLE other_table (x INTEGER)")
        conn.commit()
    finally:
        conn.close()
    db_path.chmod(0o644)
    bot = _StubBot(str(db_path))

    runner = CliRunner()
    result = _run(runner, ["--force"], bot)
    assert result.exit_code != 0
    assert result.exception is None or isinstance(result.exception, SystemExit)
    assert "unrecognized" in result.output.lower()
