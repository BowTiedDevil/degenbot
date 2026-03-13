import pathlib
import sqlite3

from alembic import command
from alembic.config import Config
from sqlalchemy import URL, Connection, Engine, create_engine, text
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.config import settings
from degenbot.database.models import Base
from degenbot.exceptions.database import BackupExists
from degenbot.logging import logger


def backup_sqlite_database(
    db_path: pathlib.Path,
    *,
    prefix: str | None = None,
    suffix: str | None = None,
    skip_confirmation: bool = False,
    engine: Engine | Connection | None = None,
) -> None:
    assert db_path.exists()

    backup_path = db_path

    if prefix is not None:
        backup_path = backup_path.with_stem(f"{prefix}-{backup_path.stem}")
    if suffix is not None:
        backup_path = backup_path.with_stem(f"{backup_path.stem}-{suffix}")

    if backup_path.exists() and not skip_confirmation:
        raise BackupExists(path=backup_path)

    if engine is None:
        # Fallback: create a new engine (legacy behavior)
        engine = create_engine(
            f"sqlite:///{db_path.absolute()}",
        )

    connection = engine if isinstance(engine, Connection) else engine.connect()

    with connection:
        # Checkpoint WAL to ensure all data is in main database
        connection.execute(
            text("PRAGMA wal_checkpoint(FULL);"),
        )

        # Get the underlying DBAPI connection for backup
        # This ensures we use the same connection pool as the active session
        raw_conn = connection.connection
        with sqlite3.connect(backup_path) as dest:
            raw_conn.backup(target=dest)

    # Verify backup integrity
    with sqlite3.connect(backup_path) as verify_conn:
        result = verify_conn.execute("PRAGMA integrity_check;").fetchone()
        assert result is not None, "Backup integrity check failed: no result"
        assert result[0] == "ok", f"Backup integrity check failed: {result[0]}"


def create_new_sqlite_database(db_path: pathlib.Path) -> None:
    engine = create_engine(
        f"sqlite:///{db_path.absolute()}",
    )
    with engine.connect() as connection:
        assert (
            connection.execute(
                text("PRAGMA journal_mode=WAL;"),
            ).scalar()
            == "wal"
        )
        connection.execute(
            text("PRAGMA auto_vacuum=FULL;"),
        )

        Base.metadata.create_all(bind=engine)
        connection.execute(
            text("VACUUM;"),
        )

        logger.info(f"Initialized new SQLite database at {db_path}")
        command.stamp(get_alembic_config(), "head")


def compact_sqlite_database(db_path: pathlib.Path) -> None:
    engine = create_engine(
        f"sqlite:///{db_path.absolute()}",
    )
    with engine.connect() as connection:
        connection.execute(
            text("VACUUM;"),
        )
        logger.info(f"Compacted SQLite database at {db_path}")


def upgrade_existing_sqlite_database() -> None:
    command.upgrade(get_alembic_config(), "head")
    logger.info("Updated existing SQLite database.")


def get_scoped_sqlite_session(database_path: pathlib.Path) -> scoped_session[Session]:
    return scoped_session(
        session_factory=sessionmaker(
            bind=create_engine(
                URL.create(
                    drivername="sqlite",
                    database=str(database_path.absolute()),
                )
            )
        )
    )


def get_alembic_config() -> Config:
    cfg = Config()
    cfg.set_main_option("sqlalchemy.url", f"sqlite:///{settings.database.path.absolute()}")
    cfg.set_main_option("script_location", "degenbot:migrations")

    return cfg
