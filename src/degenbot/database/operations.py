import pathlib
import sqlite3

from alembic import command
from alembic.config import Config
from sqlalchemy import URL, create_engine, text
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.config import settings
from degenbot.database.models import Base
from degenbot.exceptions.database import BackupExists
from degenbot.logging import logger


def backup_sqlite_database(db_path: pathlib.Path) -> None:
    assert db_path.exists()

    backup_path = pathlib.Path(db_path).with_suffix(db_path.suffix + ".bak")
    if backup_path.exists():
        raise BackupExists(path=backup_path)

    engine = create_engine(
        f"sqlite:///{db_path.absolute()}",
    )
    with engine.connect() as connection:
        connection.execute(
            text("PRAGMA wal_checkpoint(FULL);"),
        )

    with sqlite3.connect(db_path) as src, sqlite3.connect(backup_path) as dest:
        src.backup(target=dest)


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
