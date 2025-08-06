import pathlib
import sqlite3

from alembic import command
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from sqlalchemy import URL, create_engine, text
from sqlalchemy.orm import Session, scoped_session, sessionmaker

from degenbot.config import alembic_cfg, settings
from degenbot.database.models import Base
from degenbot.exceptions.database import BackupExists
from degenbot.logging import logger
from degenbot.version import __version__


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
    if db_path.exists():
        db_path.unlink()

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
        command.stamp(alembic_cfg, "head")


def vacuum_sqlite_database(db_path: pathlib.Path) -> None:
    engine = create_engine(
        f"sqlite:///{db_path.absolute()}",
    )
    with engine.connect() as connection:
        connection.execute(
            text("VACUUM;"),
        )
        logger.info(f"Defragmented SQLite database at {db_path}")


def upgrade_existing_sqlite_database() -> None:
    command.upgrade(alembic_cfg, "head")
    logger.info(f"Updated existing SQLite database at {settings.database.path.absolute()}")


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


default_session = get_scoped_sqlite_session(database_path=settings.database.path)

if default_session.connection().execute(text("PRAGMA journal_mode;")).scalar() != "wal":
    logger.warning(
        "The current database is not set to write-ahead logging (WAL). This mode provides the best "
        "performance and consistency during simultaneous reading & writing operations."
        "\n"
        "You can re-initialize the database using 'degenbot database reset'. To preserve the "
        "existing database, you may set WAL mode using 'PRAGMA journal_mode=WAL;' with the "
        "SQLite binary, or by using DB Browser for SQLite (https://sqlitebrowser.org/) "
        "or similar."
    )


current_database_version = MigrationContext.configure(
    default_session.connection()
).get_current_revision()
latest_database_version = ScriptDirectory.from_config(alembic_cfg).get_current_head()

if latest_database_version is not None and current_database_version != latest_database_version:
    logger.warning(
        f"The current database revision ({current_database_version}) does not match the latest "
        f"({latest_database_version}) for {__package__} version {__version__}!"
        "\n"
        "Database-related features may raise exceptions if you continue! Run database migrations "
        "with 'degenbot database upgrade'."
    )
