"""Database session helpers and Alembic configuration.

The SQLite **file operations** (`create_new_sqlite_database`,
`backup_sqlite_database`, `compact_sqlite_database`,
`upgrade_existing_sqlite_database`) are now thin delegating shells over the
Rust core (`degenbot._ffi.db_*`), per ADR-005 / the three-layer architecture
(ergo `OP23QV`). The Alembic-config/session helpers stay Python (config +
session orchestration are shell concerns — rubric §2.1).
"""

import pathlib
from typing import Any

from alembic import command
from alembic.config import Config
from sqlalchemy import URL, Engine, create_engine, event
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.db import (
    db_backup_database,
    db_compact_database,
    db_convert_alembic_to_rust_owned,
    db_create_new_database,
    db_heal_database,
    db_inspect_schema_state,
    db_upgrade_database,
)
from degenbot.exceptions import DatabaseSchemaStale
from degenbot.exceptions.infrastructure import BackupExists
from degenbot.logging import logger


def backup_sqlite_database(
    *,
    session: Session,
    prefix: str | None = None,
    suffix: str | None = None,
    skip_confirmation: bool = False,
) -> None:
    """Back up the SQLite database to a ``.db.bak`` sibling.

    The backup + integrity-check now run in the Rust core
    (``degenbot._ffi.db_backup_database``); this shell resolves the path,
    applies the optional ``prefix``/``suffix`` stem decoration, and honors the
    ``BackupExists`` guard (CLI orchestration concern — rubric §2.1).

    Raises:
        BackupExists: See function documentation.

    """
    session_engine = session.bind
    assert isinstance(session_engine, Engine)
    assert session_engine.url.database is not None

    source_path = pathlib.Path(session_engine.url.database).absolute()
    backup_path = source_path

    if prefix is not None:
        backup_path = backup_path.with_stem(f"{prefix}-{backup_path.stem}")
    if suffix is not None:
        backup_path = backup_path.with_stem(f"{backup_path.stem}-{suffix}")

    backup_path = backup_path.with_suffix(".db.bak")

    if backup_path.exists() and not skip_confirmation:
        raise BackupExists(path=backup_path)

    db_backup_database(str(source_path), str(backup_path))
    logger.info(f"Backed up SQLite database to {backup_path}")


def _get_sqlite_db_string(db_path: pathlib.Path) -> str:
    """Get the SQLite database string for a path, handling :memory: specially.

    Returns:
        The computed string value.

    """
    if db_path.name == ":memory:":
        return ":memory:"
    return str(db_path.absolute())


def create_new_sqlite_database(db_path: pathlib.Path) -> None:
    """Create a new SQLite database stamped at the Alembic head.

    Delegates to the Rust core (``degenbot._ffi.db_create_new_database``):
    WAL mode + the full head DDL + ``VACUUM`` + an Alembic ``head`` stamp.
    """
    db_create_new_database(str(db_path))
    logger.info(f"Initialized new SQLite database at {db_path}")


def compact_sqlite_database(db_path: pathlib.Path) -> None:
    """Compact the SQLite database via ``VACUUM``.

    Delegates to the Rust core (``degenbot._ffi.db_compact_database").
    """
    db_compact_database(str(db_path))
    logger.info(f"Compacted SQLite database at {db_path}")


def upgrade_existing_sqlite_database(database_path: pathlib.Path) -> str:
    """Ensure the SQLite database is at the latest schema.

    Two-stage upgrade path:

    1. **Rust fast-path** (``degenbot._ffi.db_upgrade_database``): a no-op if
       already at the Alembic head, or applies the head DDL + stamp on an
       empty file. This handles the common cases without spinning up the
       Alembic migration runner.
    2. **Alembic fall-back**: if the Rust core reports the DB is stamped at a
       *prior* Alembic revision (``AlembicStale`` — e.g. a user upgrading from
       the published 0.6.0a2 schema at ``e0aaad8ad486``), run
       ``alembic upgrade head`` from Python to apply the migration scripts.
       The Rust core never runs Alembic migrations; this shell does.

    Returns:
        The Rust outcome string (``"already_at_head"`` / ``"created_fresh"``)
        for the fast-path cases, or ``"upgraded_from_stale"`` when the Alembic
        fall-back ran the migration scripts.

    Note:
        A ``DatabaseSchemaStale`` from the Rust fast-path is caught here so the
        Alembic fall-back can run; it does not propagate. An unrecognised schema
        re-raises as ``ValueError`` from the Rust core call below.

    """
    try:
        outcome = db_upgrade_database(str(database_path))
    except DatabaseSchemaStale:
        logger.info(
            f"Database at {database_path} is at a prior Alembic revision; "
            "running `alembic upgrade head` to apply migration scripts.",
        )
        command.upgrade(get_alembic_config(database_path=database_path), "head")
        outcome = "upgraded_from_stale"
    logger.info(f"Updated existing SQLite database at {database_path} ({outcome}).")
    return outcome


def convert_alembic_to_rust_owned(database_path: pathlib.Path) -> str:
    """Flip an Alembic-stamped DB into Rust ownership (ADR-010 cutover).

    A thin delegating shell over the Rust core
    (``degenbot._ffi.db_convert_alembic_to_rust_owned``): drops
    ``alembic_version``, stamps ``_degenbot_db_schema_version``. The cutover is
    one-way and opt-in — nothing on the open path auto-runs it.

    Unlike :func:`upgrade_existing_sqlite_database`, there is NO Alembic
    fall-back: the cutover REFUSES a stale Alembic DB (the user must run
    :func:`upgrade_existing_sqlite_database` first to advance the stamp to
    head). ``DatabaseSchemaStale`` and an unrecognized-schema ``ValueError``
    propagate to the caller.

    Returns:
        ``"converted"`` (was AlembicCurrent) or ``"already_rust_owned"``
        (was already Rust-owned → idempotent no-op).

    """
    outcome = db_convert_alembic_to_rust_owned(str(database_path))
    logger.info(f"Database at {database_path}: schema ownership cutover ({outcome}).")
    return outcome


def inspect_schema_state(database_path: pathlib.Path) -> str:
    """Inspect the schema state WITHOUT writing (ADR-010 dry-run).

    Thin shell over ``degenbot._ffi.db_inspect_schema_state`` — the read-only
    companion to :func:`convert_alembic_to_rust_owned`. Reports the state for
    all cases (never refuses); only a genuine I/O failure raises.

    Returns:
        One of ``"alembic_current"`` / ``"alembic_stale"`` /
        ``"fresh_standalone"`` / ``"rust_owned"`` / ``"unrecognized"``.

    """
    return db_inspect_schema_state(str(database_path))


def heal_database(database_path: pathlib.Path) -> dict[str, Any]:
    """Out-of-place dump-and-restore heal (ADR-011).

    Thin shell over ``degenbot._ffi.db_heal_database``: rebuilds the DB at the
    Rust head schema, copies all user rows preserving PKs + FK integrity,
    stamps RustOwned, atomic-swaps with a ``*.bak`` backup. Never mutates
    the old DB in place. No Alembic dependency (the column mapping is
    auto-derived in Rust).

    Unlike :func:`convert_alembic_to_rust_owned`, heal ACCEPTS a stale Alembic
    DB (the out-of-place rebuild handles divergent schemas via the
    auto-derived column mapping). An unrecognized (foreign) DB is refused
    (raises ``ValueError`` from the Rust core).

    Returns:
        The heal report dict: ``{old_state, rows_copied, bak_path,
        new_state, warnings}``. No-op if old is already ``rust_owned``.

    """
    report = db_heal_database(str(database_path))
    logger.info(f"Database at {database_path}: heal ({report['new_state']}).")
    return report


def get_scoped_sqlite_session(database_path: pathlib.Path) -> scoped_session[Session]:
    """Return scoped sqlite session.

    Concurrency note: every pooled connection is opened with WAL journal mode,
    a 5s ``busy_timeout``, and ``synchronous=NORMAL`` via a ``"connect"`` event
    listener. WAL is file-persistent once set (cheap to re-assert); the
    per-connection ``busy_timeout`` and ``synchronous`` must be re-asserted on
    each pooled connection so concurrent readers/writers degrade gracefully to
    a retry-with-timeout instead of an immediate ``SQLITE_BUSY``.

    Returns:
        The computed value.

    """
    engine = create_engine(
        URL.create(
            drivername="sqlite",
            database=_get_sqlite_db_string(database_path),
        ),
    )

    @event.listens_for(engine, "connect")
    def _set_sqlite_pragmas(dbapi_connection, _connection_record) -> None:  # ruff:ignore[missing-type-function-argument]
        cursor = dbapi_connection.cursor()
        cursor.execute("PRAGMA journal_mode=WAL;")
        cursor.execute("PRAGMA busy_timeout=5000;")
        cursor.execute("PRAGMA synchronous=NORMAL;")
        cursor.close()

    return scoped_session(
        session_factory=sessionmaker(
            bind=engine,
        ),
    )


def get_alembic_config(database_path: pathlib.Path | None = None) -> Config:
    """Return alembic config.

    Returns:
        The computed value.

    Raises:
        ValueError: See function documentation.

    """
    cfg = Config()
    if database_path is None:
        msg = "database_path is required. Pass it explicitly or use Bot.config.database.path"
        raise ValueError(msg)
    cfg.set_main_option("sqlalchemy.url", f"sqlite:///{_get_sqlite_db_string(database_path)}")
    cfg.set_main_option("script_location", "degenbot:migrations")

    return cfg
