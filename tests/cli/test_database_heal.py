"""CLI tests for `degenbot database heal` (ADR-011, ergo task WEDVGE).

The heal command is the out-of-place dump-and-restore path to Rust schema
ownership. It surfaces the core substrate from FCA2HP (`ca782237`) —
`degenbot_db::heal::heal_database` — via the PyO3 seam from MZ55NP
(`67691f7c` — `degenbot._ffi.db_heal_database`) and a Click command.

Unlike `database cutover`, heal ACCEPTS a stale Alembic DB (the out-of-place
rebuild handles divergent schemas via the auto-derived column mapping) — so
it is the rugpull-proof retirement path: any old DB can be brought to Rust
ownership without a working Alembic migration chain. A foreign (unrecognized)
SQLite file is refused.

Cases:
- `--dry-run` on an AlembicCurrent DB → prints the state + "would heal",
  writes nothing (still AlembicCurrent on re-inspect, no `.bak`).
- `--force` heal on AlembicCurrent (with a pre-written exchange row) →
  `.bak` exists; live DB `rust_owned`; the exchange row STILL reads back
  (rugpull-protection proof — same as the cutover boundary test, but for heal).
- `--force` heal on a stale DB → heal accepts stale → RustOwned + index back
  + rows preserved (the headline difference from cutover, which refuses stale).
- `--force` on an already-RustOwned DB → no-op (no copy, no `.bak`).
- `--dry-run` on an unrecognized file → reports unrecognized, exit 0.
- `--force` on an unrecognized file → friendly message + non-zero exit, no
  traceback.
- No `--force`, confirm prompt declines → `click.Abort` (no-op).
- `--force` leaves the `.bak` holding the OLD data (recoverability proof).
"""

from __future__ import annotations

import pathlib
import sqlite3
from types import SimpleNamespace

import pytest
from click.testing import CliRunner

from degenbot.cli.database import database_heal
from degenbot.database.operations import (
    convert_alembic_to_rust_owned,
    create_new_sqlite_database,
    get_scoped_sqlite_session,
)
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot._ffi import (
    db_fetch_exchange_by_name,
    db_inspect_schema_state,
    db_upsert_exchange,
)

# The Alembic head revision and its direct parent, as stamped by the Rust
# core (`degenbot_db::schema::ALEMBIC_HEAD`). See
# `rust/crates/degenbot-db/src/schema.rs`.
_ALEMBIC_HEAD = "2606a6c7f5ee"
_ALEMBIC_HEAD_PARENT = "e0aaad8ad486"


class _StubBot:
    """Stub bot exposing the attributes the database heal command touches.

    `database_heal` only reads `bot.config.database.path` (it never opens
    `bot.db()` — the heal op opens its own connection in the Rust core).
    """

    def __init__(self, path: str) -> None:
        self.config = SimpleNamespace(database=SimpleNamespace(path=path))
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=pathlib.Path(path)),
        )


@pytest.fixture
def fresh_alembic_db(tmp_path: pathlib.Path) -> _StubBot:
    """A freshly-created DB stamped at the Alembic head (AlembicCurrent)."""
    db_path = tmp_path / "alembic.db"
    create_new_sqlite_database(db_path)
    db_path.chmod(0o644)
    return _StubBot(str(db_path))


@pytest.fixture
def consistent_stale_alembic_db(fresh_alembic_db: _StubBot) -> _StubBot:
    """A DB genuinely at the revision just below the Alembic head.

    Built FROM ``fresh_alembic_db`` (head schema + head stamp) by reversing
    the head migration's single effect (dropping ``ix_erc20_tokens_chain``)
    and rewriting the stamp row back to ``e0aaad8ad486`` — the same
    construction as the cutover boundary test's fixture (the genuine end
    state of a 0.6.x user's DB before their final upgrade).
    """
    db_path = fresh_alembic_db.config.database.path
    conn = sqlite3.connect(str(db_path))
    try:
        conn.execute("DROP INDEX ix_erc20_tokens_chain")
        conn.execute(
            "UPDATE alembic_version SET version_num=?",
            (_ALEMBIC_HEAD_PARENT,),
        )
        conn.commit()
    finally:
        conn.close()
    return fresh_alembic_db


@pytest.fixture
def rust_owned_db(fresh_alembic_db: _StubBot) -> _StubBot:
    """A DB already flipped to Rust ownership (RustOwned)."""
    convert_alembic_to_rust_owned(database_path=fresh_alembic_db.config.database.path)
    assert db_inspect_schema_state(fresh_alembic_db.config.database.path) == "rust_owned"
    return fresh_alembic_db


def _run(runner: CliRunner, args: list[str], bot: _StubBot):
    return runner.invoke(database_heal, args, obj=bot, catch_exceptions=False)


def _run_catching(runner: CliRunner, args: list[str], bot: _StubBot):
    # For the refusal cases we WANT Click to catch the SystemExit (so exit_code
    # is set and result.output carries the echo) rather than propagating it
    # out as an exception.
    return runner.invoke(database_heal, args, obj=bot, catch_exceptions=True)


def _index_exists(db_path: str, name: str) -> bool:
    conn = sqlite3.connect(db_path)
    try:
        return (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?",
                (name,),
            ).fetchone()[0]
            == 1
        )
    finally:
        conn.close()


def _bak_path_for(db_path: str | pathlib.Path) -> pathlib.Path:
    return pathlib.Path(str(db_path) + ".bak")


def _count_exchanges(db_path: str) -> int:
    conn = sqlite3.connect(db_path)
    try:
        return conn.execute("SELECT COUNT(*) FROM exchanges").fetchone()[0]
    finally:
        conn.close()


# ── 1. dry-run writes nothing ─────────────────────────────────────────────


def test_dry_run_writes_nothing(fresh_alembic_db: _StubBot) -> None:
    runner = CliRunner()
    before = db_inspect_schema_state(fresh_alembic_db.config.database.path)
    assert before == "alembic_current"

    result = _run(runner, ["--dry-run"], fresh_alembic_db)
    assert result.exit_code == 0, result.output
    assert "alembic_current" in result.output
    assert "would heal" in result.output.lower()

    # wrote nothing — still AlembicCurrent, no .bak file created.
    after = db_inspect_schema_state(fresh_alembic_db.config.database.path)
    assert after == "alembic_current"
    assert not _bak_path_for(fresh_alembic_db.config.database.path).exists()


# ── 2. force heal on AlembicCurrent preserves data ───────────────────────


# Reuse the Uniswap V2 mainnet factory as the cutover boundary test.
_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"


def test_force_heal_on_alembic_current_preserves_exchange_row(
    fresh_alembic_db: _StubBot,
) -> None:
    db_path = fresh_alembic_db.config.database.path
    db_path_str = str(db_path)

    # Write an exchange row via the Rust write seam.
    row = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
        factory=_FACTORY,
        deployer=None,
    )
    assert row.id is not None

    runner = CliRunner()
    result = _run(runner, ["--force"], fresh_alembic_db)
    assert result.exit_code == 0, result.output
    assert "healed" in result.output.lower()

    # .bak exists; live DB is now Rust-owned.
    assert _bak_path_for(db_path).exists()
    assert db_inspect_schema_state(db_path_str) == "rust_owned"

    # The exchange row STILL reads back on the healed (live) DB — data survives
    # the out-of-place dump-and-restore (rugpull-protection proof).
    fetched = db_fetch_exchange_by_name(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
    )
    assert fetched is not None
    assert fetched.id == row.id


# ── 3. force heal on a STALE DB (heal accepts stale — unlike cutover) ─────


def test_force_heal_on_stale_accepts_and_rebuilds(
    consistent_stale_alembic_db: _StubBot,
) -> None:
    db_path = consistent_stale_alembic_db.config.database.path
    db_path_str = str(db_path)

    # Pre-conditions: genuinely stale, index absent.
    assert db_inspect_schema_state(db_path_str) == "alembic_stale"
    assert not _index_exists(db_path_str, "ix_erc20_tokens_chain")

    runner = CliRunner()
    result = _run(runner, ["--force"], consistent_stale_alembic_db)
    assert result.exit_code == 0, result.output
    assert "healed" in result.output.lower()

    # Healed → RustOwned; the head DDL re-applied → the index is back.
    assert db_inspect_schema_state(db_path_str) == "rust_owned"
    assert _index_exists(db_path_str, "ix_erc20_tokens_chain")
    # alembic_version is gone.
    conn = sqlite3.connect(db_path_str)
    try:
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='alembic_version'",
            ).fetchone()[0]
            == 0
        )
    finally:
        conn.close()


# ── 4. already RustOwned → no-op ─────────────────────────────────────────


def test_force_heal_on_already_rustowned_is_noop(rust_owned_db: _StubBot) -> None:
    db_path = rust_owned_db.config.database.path
    db_path_str = str(db_path)
    # No .bak before heal.
    assert not _bak_path_for(db_path).exists()

    runner = CliRunner()
    result = _run(runner, ["--force"], rust_owned_db)
    assert result.exit_code == 0, result.output
    assert "already rust-owned" in result.output.lower()

    # No-op: no copy, no .bak, live DB untouched.
    assert not _bak_path_for(db_path).exists()
    assert db_inspect_schema_state(db_path_str) == "rust_owned"


# ── 5. dry-run on unrecognized reports (exit 0) ───────────────────────────


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


# ── 6. force on unrecognized exits nonzero, no traceback ─────────────────


def test_force_heal_on_unrecognized_exits_nonzero(tmp_path: pathlib.Path) -> None:
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
    result = _run_catching(runner, ["--force"], bot)
    assert result.exit_code != 0
    # SystemExit(1) is caught by the runner (not a raw traceback).
    assert result.exception is None or isinstance(result.exception, SystemExit)
    assert "unrecognized" in result.output.lower()


# ── 7. confirm prompt default declines → Abort ───────────────────────────


def test_confirm_prompt_default_declines(fresh_alembic_db: _StubBot) -> None:
    db_path_str = str(fresh_alembic_db.config.database.path)
    runner = CliRunner()
    # No input → confirm defaults to False → click.Abort.
    result = _run_catching(runner, [], fresh_alembic_db)
    assert result.exit_code != 0
    # Abort surfaces cleanly (not a traceback).
    assert result.exception is None or isinstance(
        result.exception,
        SystemExit | __import__("click").exceptions.Abort,
    )
    # Live DB untouched.
    assert db_inspect_schema_state(db_path_str) == "alembic_current"
    assert not _bak_path_for(db_path_str).exists()


# ── 8. .bak holds the OLD data (recoverability proof) ────────────────────


def test_force_heal_leaves_bak_with_old_data(fresh_alembic_db: _StubBot) -> None:
    db_path = fresh_alembic_db.config.database.path
    db_path_str = str(db_path)

    # Write an exchange row BEFORE the heal.
    db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
        factory=_FACTORY,
        deployer=None,
    )
    assert _count_exchanges(db_path_str) == 1

    runner = CliRunner()
    result = _run(runner, ["--force"], fresh_alembic_db)
    assert result.exit_code == 0, result.output

    # The .bak exists and holds the OLD data (1 exchange row — the pre-heal
    # count). Full recoverability: the backup is a complete readable snapshot.
    bak = _bak_path_for(db_path)
    assert bak.exists()
    assert _count_exchanges(str(bak)) == 1
    # And it retains the OLD Alembic ownership shape.
    assert db_inspect_schema_state(str(bak)) == "alembic_current"
