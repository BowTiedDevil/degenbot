"""Database session helpers and Alembic configuration.

The SQLite **file operations** (`create_new_sqlite_database`,
`backup_sqlite_database`, `compact_sqlite_database`,
`upgrade_existing_sqlite_database`) are now thin delegating shells over the
Rust core (`degenbot_rs.db_*`), per ADR-005 / the three-layer architecture
(ergo `OP23QV`). The Alembic-config/session helpers stay Python (config +
session orchestration are shell concerns — rubric §2.1).
"""

import pathlib

from alembic.config import Config
from sqlalchemy import URL, Engine, create_engine, event
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.degenbot_rs import (
    db_backup_database,
    db_compact_database,
    db_create_new_database,
    db_upgrade_database,
)
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
    (``degenbot_rs.db_backup_database``); this shell resolves the path,
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

    Delegates to the Rust core (``degenbot_rs.db_create_new_database``):
    WAL mode + the full head DDL + ``VACUUM`` + an Alembic ``head`` stamp.
    """
    db_create_new_database(str(db_path))
    logger.info(f"Initialized new SQLite database at {db_path}")


def compact_sqlite_database(db_path: pathlib.Path) -> None:
    """Compact the SQLite database via ``VACUUM``.

    Delegates to the Rust core (``degenbot_rs.db_compact_database").
    """
    db_compact_database(str(db_path))
    logger.info(f"Compacted SQLite database at {db_path}")


def upgrade_existing_sqlite_database(database_path: pathlib.Path) -> str:
    """Ensure the SQLite database is at the latest schema.

    Delegates to the Rust core (``degenbot_rs.db_upgrade_database``): a no-op
    if already at the Alembic head, or applies the head DDL + stamp on an empty
    file. A stale Alembic DB raises ``ValueError`` (run
    ``alembic upgrade head`` from Python to cross migration revisions).

    Returns:
        The Rust outcome string (e.g. ``"already_at_head"``).

    """
    outcome = db_upgrade_database(str(database_path))
    logger.info(f"Updated existing SQLite database at {database_path} ({outcome}).")
    return outcome


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
    def _set_sqlite_pragmas(dbapi_connection, _connection_record) -> None:  # noqa: ANN001
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
