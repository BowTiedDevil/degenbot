import click

from degenbot.bot import Bot
from degenbot.cli import cli
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
@click.pass_obj
def database_backup(bot: Bot) -> None:
    """Back up the database."""

    try:
        with bot.db() as session:
            backup_sqlite_database(
                session=session,
            )
    except BackupExists as exc:
        user_confirm = click.confirm(
            f"An existing backup was found at {exc.path}. Do you want to replace it?",
            default=False,
        )
        if user_confirm:
            exc.path.unlink(missing_ok=True)
            with bot.db() as session:
                backup_sqlite_database(
                    session=session,
                )
        else:
            raise click.Abort from None


@database.command("reset", hidden=True)
@click.option(
    "--force",
    is_flag=True,
    help="Skip confirmation prompt",
)
@click.pass_obj
def database_reset(bot: Bot, *, force: bool) -> None:
    """
    Remove and recreate the database.
    """

    if force or click.confirm(
        f"The existing database at {bot.config.database.path} will be removed and a new, empty database will be created and initialized using the schema included in {__package__} version {__version__}. Do you want to proceed?",  # noqa: E501
        default=False,
    ):
        bot.config.database.path.unlink(missing_ok=True)
        create_new_sqlite_database(bot.config.database.path)
    else:
        raise click.Abort


@database.command("upgrade")
@click.option(
    "--force",
    is_flag=True,
    help="Skip confirmation prompt",
)
@click.pass_obj
def database_upgrade(bot: Bot, *, force: bool) -> None:
    """
    Upgrade the database to the latest schema.
    """

    if force or click.confirm(
        f"The database at {bot.config.database.path} will be upgraded from version {current_database_version} to {latest_database_version}. Do you want to proceed?",  # noqa:E501
        default=False,
    ):
        upgrade_existing_sqlite_database()
    else:
        raise click.Abort


@database.command("compact")
@click.pass_obj
def database_compact(bot: Bot) -> None:
    """
    Compact the database.
    """
    compact_sqlite_database(bot.config.database.path)
