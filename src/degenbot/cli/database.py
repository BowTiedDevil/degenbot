import click

from degenbot.cli import cli
from degenbot.config import settings
from degenbot.database import current_database_version, latest_database_version
from degenbot.database.operations import (
    backup_sqlite_database,
    compact_sqlite_database,
    create_new_sqlite_database,
    upgrade_existing_sqlite_database,
)
from degenbot.exceptions.database import BackupExists
from degenbot.version import __version__


@cli.group()
def database() -> None:
    """
    Database commands
    """


@database.command("backup")
def database_backup() -> None:
    """
    Back up the database.
    """

    try:
        backup_sqlite_database(settings.database.path)
    except BackupExists as exc:
        user_confirm = click.confirm(
            f"An existing backup was found at {exc.path}. Do you want to remove it and continue?",
            default=False,
        )
        if user_confirm:
            exc.path.unlink()
            backup_sqlite_database(settings.database.path)
        else:
            raise click.Abort from None


@database.command("reset")
def database_reset() -> None:
    """
    Remove and recreate the database.
    """

    user_confirm = click.confirm(
        f"The existing database at {settings.database.path} will be removed and a new, empty database will be created and initialized using the schema included in {__package__} version {__version__}. Do you want to proceed?",  # noqa: E501
        default=False,
    )
    if user_confirm:
        settings.database.path.unlink()
        create_new_sqlite_database(settings.database.path)
    else:
        raise click.Abort


@database.command("upgrade")
@click.option(
    "--force",
    is_flag=True,
    help="Skip confirmation prompt",
)
def database_upgrade(*, force: bool) -> None:
    """
    Upgrade the database to the latest schema.
    """

    if force or click.confirm(
        f"The database at {settings.database.path} will be upgraded from version {current_database_version} to {latest_database_version}. Do you want to proceed?",  # noqa:E501
        default=False,
    ):
        upgrade_existing_sqlite_database()
    else:
        raise click.Abort


@database.command("compact")
def database_compact() -> None:
    """
    Compact the database.
    """
    compact_sqlite_database(settings.database.path)
