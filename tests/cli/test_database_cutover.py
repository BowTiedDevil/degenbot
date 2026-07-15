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

import pathlib
import sqlite3
from types import SimpleNamespace

import pytest
from click.testing import CliRunner

from degenbot.cli.database import database_cutover, database_upgrade
from degenbot.database.operations import (
    create_new_sqlite_database,
    get_scoped_sqlite_session,
)
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.database._ffi import (
    db_fetch_exchange_by_name,
    db_inspect_schema_state,
    db_upsert_exchange,
)

# The Alembic head revision and its direct parent, as stamped by the Rust
# core (`degenbot_db::schema::ALEMBIC_HEAD`). The head migration
# `2606a6c7f5ee_erc20_add_chain_id_index` has exactly one effect — creating
# `ix_erc20_tokens_chain` — so a DB stamped at the parent revision
# `e0aaad8ad486` differs from the head schema only by the absence of that
# index. See `rust/crates/degenbot-db/src/schema.rs` for the Rust constant.
_ALEMBIC_HEAD = "2606a6c7f5ee"
_ALEMBIC_HEAD_PARENT = "e0aaad8ad486"


class _StubBot:
    """Stub bot exposing the attributes the database CLI commands touch.

    `database_cutover` only reads `bot.config.database.path`. `database_upgrade`
    additionally calls `bot.db()` (as a context manager) and `bot.db.connection()`
    to read the current Alembic revision before its `--force` short-circuit, so
    we attach a real ``DatabaseSessionManager`` over the file. The autouse
    ``dispose_all`` teardown in ``tests/conftest.py`` releases the engine.
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


def _run(runner: CliRunner, args: list[str], bot: _StubBot):
    return runner.invoke(database_cutover, args, obj=bot, catch_exceptions=False)


def _fetch_stamp(db_path: str) -> str | None:
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute("SELECT version_num FROM alembic_version").fetchone()
    finally:
        conn.close()
    return None if row is None else row[0]


def _table_exists(db_path: str, name: str) -> bool:
    conn = sqlite3.connect(db_path)
    try:
        return (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                (name,),
            ).fetchone()[0]
            == 1
        )
    finally:
        conn.close()


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
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='alembic_version'",
            ).fetchone()[0]
            == 1
        )
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
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='alembic_version'",
            ).fetchone()[0]
            == 0
        )
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


# ── end-to-end boundary: stale → upgrade → cutover (ADR-010, ergo YWN7Z6) ──


@pytest.fixture
def consistent_stale_alembic_db(fresh_alembic_db: _StubBot) -> _StubBot:
    """A DB genuinely at the revision just below the Alembic head.

    Built FROM ``fresh_alembic_db`` (head schema + head stamp) by reversing
    the head migration's single effect (dropping ``ix_erc20_tokens_chain``)
    and rewriting the stamp row back to ``e0aaad8ad486``. The result is
    byte-identical to what a real incremental upgrader's DB would be in after
    their last 0.6.x-point-release upgrade: head-minus-one schema + the older
    stamp.

    We do NOT build this via ``alembic upgrade e0aaad8ad486`` from an empty
    file — the forward migration chain is not buildable from scratch
    (``87fd9fc7ae00_update_uniswap_v4_table_indices`` does an unguarded
    ``op.drop_index("ix_managed_pool_hash")`` on an index no prior migration
    creates when run forward), nor via ``alembic downgrade`` (the head
    migration raises ``NotImplementedError``). These are PRE-EXISTING
    migration-chain limitations on the 0.6.x kill-list
    (``src/degenbot/migrations/``), fenced off from this epic; realistic users
    apply migrations incrementally and so never hit forward-build fragility.
    The reconstruction here reproduces the genuine end state without touching
    the broken chain.
    """
    # `database upgrade` routes `bot.config.database.path` into
    # `get_alembic_config` -> `_get_sqlite_db_string`, which calls `.name` and so
    # requires a `pathlib.Path` (matching the real `DegenbotConfig.database.path`
    # type). The other stubs keep a `str` path because they only invoke the
    # cutover command (which stringifies internally) and raw Rust seams (str).
    fresh_alembic_db.config.database.path = pathlib.Path(
        fresh_alembic_db.config.database.path,
    )
    db_path = fresh_alembic_db.config.database.path
    conn = sqlite3.connect(db_path)
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


def test_alembic_to_rust_cutover_boundary_end_to_end(
    consistent_stale_alembic_db: _StubBot,
) -> None:
    """Prove the full Alembic→Rust cutover boundary end-to-end (ADR-010 §3).

    This is the contract artifact for the schema-ownership transition — the
    path ``pip`` users take across the 0.6.x → 0.7 boundary: they
    ``pip install --upgrade degenbot``, run ``degenbot database upgrade``
    (advancing their older-stamped DB through the final Alembic revision via
    the Alembic fall-back), then ``degenbot database cutover`` to flip schema
    ownership into Rust. The machinery all shipped in HTG7SL (``020dffdf``)
    and FF5YGT (``1b66da1f``); this test only exercises it.

    Journey:
      1. Start from a DB genuinely stamped one revision below the head
         (``consistent_stale_alembic_db`` fixture) → ``alembic_stale``.
      2. ``degenbot database upgrade --force`` advances it to the Alembic
         head via the real Alembic fall-back (the Rust fast-path refuses
         ``AlembicStale`` → the shell catches ``DatabaseSchemaStale`` → runs
         ``alembic upgrade head``, which re-creates ``ix_erc20_tokens_chain``
         and re-stamps to ``2606a6c7f5ee``).
      3. On the now-current DB: schema state is ``alembic_current``, the
         ``alembic_version`` table is present, and a Rust write
         (``db_upsert_exchange``) round-trips through the Rust seam.
      4. ``degenbot database cutover --force`` → "converted".
      5. Re-inspect: ``rust_owned``; ``alembic_version`` table GONE;
         ``_degenbot_db_schema_version`` stamped; the exchange row written in
         step 3 STILL reads back (data survives the cutover).

    The 0.7 retirement epic (TGIP5N, successor to JFFQV2) removes only the
    Alembic-side of steps 1-2 via the auto-heal mechanism; this test's steps
    3-5 (cutover + ``RustOwned``) survive retirement.
    """
    bot = consistent_stale_alembic_db
    db_path = bot.config.database.path  # a pathlib.Path (see fixture)
    # Raw Rust PyO3 seams take `str`; the CLI commands stringify internally.
    db_path_str = str(db_path)
    runner = CliRunner()

    # Step 1: the fixture left the DB genuinely stale at the parent revision.
    assert db_inspect_schema_state(db_path_str) == "alembic_stale"
    assert _fetch_stamp(db_path_str) == _ALEMBIC_HEAD_PARENT
    assert not _index_exists(db_path_str, "ix_erc20_tokens_chain")

    # Step 2: `database upgrade --force` advances via the Alembic fall-back.
    upgrade_result = runner.invoke(
        database_upgrade,
        ["--force"],
        obj=bot,
        catch_exceptions=False,
    )
    assert upgrade_result.exit_code == 0, upgrade_result.output
    # `database_upgrade` opens a SQLAlchemy session via `bot.db()` (to read the
    # current revision before the `--force` short-circuit). The scoped session
    # keeps the underlying ``sqlite3`` connection pooled, which holds the WAL/
    # shm lock and would make the raw ``sqlite3.connect`` assertions below fail
    # with ``disk I/O error``. Release the session + dispose the engine so the
    # remaining assertions (raw ``sqlite3`` reads + Rust seams, none of which
    # use ``bot.db``) can connect. This is a test-harness concern; the cutover
    # machinery (steps 4-5) does not touch ``bot.db``.
    bot.db.remove()
    bot.db.dispose()
    assert db_inspect_schema_state(db_path_str) == "alembic_current"
    assert _fetch_stamp(db_path_str) == _ALEMBIC_HEAD
    assert _index_exists(db_path_str, "ix_erc20_tokens_chain")
    assert _table_exists(db_path_str, "alembic_version")

    # Step 3: Rust write/read round-trips on the Alembic-current DB.
    factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"  # Uniswap V2 mainnet
    row = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
        factory=factory,
        deployer=None,
    )
    assert row.id is not None
    assert (
        db_fetch_exchange_by_name(
            database_path=db_path_str,
            chain_id=1,
            name="uniswap_v2",
        ).id
        == row.id
    )

    # Step 4: cutover flips schema ownership into Rust.
    cutover_result = _run(runner, ["--force"], bot)
    assert cutover_result.exit_code == 0, cutover_result.output
    assert "Cut over" in cutover_result.output

    # Step 5: RustOwned, alembic_version dropped, schema-version table stamped,
    # and the exchange row from step 3 still reads back (data survives).
    assert db_inspect_schema_state(db_path_str) == "rust_owned"
    assert not _table_exists(db_path_str, "alembic_version")
    assert _table_exists(db_path_str, "_degenbot_db_schema_version")
    assert (
        db_fetch_exchange_by_name(
            database_path=db_path_str,
            chain_id=1,
            name="uniswap_v2",
        ).id
        == row.id
    )
