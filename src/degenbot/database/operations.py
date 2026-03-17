import pathlib
import sqlite3

from alembic import command
from alembic.config import Config
from sqlalchemy import URL, Engine, create_engine, text
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.config import settings
from degenbot.database.models import Base
from degenbot.exceptions.database import BackupExists
from degenbot.logging import logger


def backup_sqlite_database(
    *,
    session: Session,
    prefix: str | None = None,
    suffix: str | None = None,
    skip_confirmation: bool = False,
) -> None:
    session_engine = session.bind
    assert isinstance(session_engine, Engine)
    assert session_engine.url.database is not None

    backup_path = pathlib.Path(session_engine.url.database).absolute()

    if prefix is not None:
        backup_path = backup_path.with_stem(f"{prefix}-{backup_path.stem}")
    if suffix is not None:
        backup_path = backup_path.with_stem(f"{backup_path.stem}-{suffix}")

    backup_path = backup_path.with_suffix(".db.bak")

    if backup_path.exists() and not skip_confirmation:
        raise BackupExists(path=backup_path)

    # Get the underlying DBAPI connection for backup
    with sqlite3.connect(backup_path) as backup_connection:
        raw_conn = session.connection().connection.driver_connection
        assert isinstance(raw_conn, sqlite3.Connection)
        raw_conn.backup(target=backup_connection)

        # Verify integrity of underlying database
        [(result,)] = raw_conn.execute("PRAGMA integrity_check;").fetchall()
        assert result == "ok", f"Backup integrity check failed: {result=}"

        # Verify backup integrity
        [(result,)] = backup_connection.execute("PRAGMA integrity_check;").fetchall()
        assert result == "ok", f"Backup integrity check failed: {result=}"


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
