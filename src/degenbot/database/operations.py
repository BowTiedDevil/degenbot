"""Database session helpers and SQLite file operations.

The SQLite **file operations** (`create_new_sqlite_database`,
`backup_sqlite_database`, `compact_sqlite_database`, `heal_database`) are thin
delegating shells over the Rust core (`degenbot._ffi.db_*`), per ADR-005 / the
three-layer architecture (ergo `OP23QV`). Session orchestration stays Python
(shell concern — rubric §2.1); the schema is Rust-owned and upgrades itself at
open (ADR-052).
"""

import pathlib
from collections.abc import Iterable
from typing import Any

from sqlalchemy import URL, Engine, create_engine, event, select
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.db import (
    db_backup_database,
    db_compact_database,
    db_create_new_database,
    db_heal_database,
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
    """Create a new SQLite database at the current Rust-owned schema head.

    Delegates to the Rust core (``degenbot._ffi.db_create_new_database``):
    WAL mode + the full head DDL + ``VACUUM`` + the private
    ``_degenbot_db_schema_version`` stamp.
    """
    db_create_new_database(str(db_path))
    logger.info(f"Initialized new SQLite database at {db_path}")


def compact_sqlite_database(db_path: pathlib.Path) -> None:
    """Compact the SQLite database via ``VACUUM``.

    Delegates to the Rust core (``degenbot._ffi.db_compact_database``).
    """
    db_compact_database(str(db_path))
    logger.info(f"Compacted SQLite database at {db_path}")


def heal_database(database_path: pathlib.Path) -> dict[str, Any]:
    """Out-of-place dump-and-restore heal (ADR-011).

    Thin shell over ``degenbot._ffi.db_heal_database``: rebuilds the DB at the
    Rust head schema, copies all user rows preserving PKs + FK integrity,
    stamps Rust ownership, atomic-swaps with a ``*.bak`` backup. Never mutates
    the old DB in place. The column mapping is auto-derived in the Rust core.

    An unrecognized (foreign) DB is refused (raises ``ValueError`` from the
    Rust core).

    Returns:
        The heal report dict: ``{old_state, rows_copied, bak_path,
        new_state, warnings}``. No-op if the source is already Rust-owned.

    """
    report = db_heal_database(str(database_path))
    logger.info(f"Database at {database_path}: heal ({report['new_state']}).")
    return report


def resolve_token_ids(
    chain_id: int,
    addresses: Iterable[str],
    session: Session,
) -> dict[str, int]:
    """Map token addresses to their row ids for `chain_id`.

    ORM query helper kept in the database layer so the pathfinding module
    stays free of SQLAlchemy (ZNWXNC). Addresses not present in the
    database are omitted from the mapping.

    Returns:
        ``{address: row_id}`` for the found tokens; empty for an
        empty input.

    """
    addresses = set(addresses)
    if not addresses:
        return {}
    rows = session.execute(
        select(Erc20TokenTable.address, Erc20TokenTable.id).where(
            Erc20TokenTable.address.in_(addresses),
            Erc20TokenTable.chain == chain_id,
        ),
    ).all()
    return {str(addr): int(tid) for addr, tid in rows}


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
