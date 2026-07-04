"""CLI commands for database inspection."""

import click
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory

from degenbot.bot import Bot
from degenbot.cli import cli
from degenbot.database.operations import (
    backup_sqlite_database,
    compact_sqlite_database,
    convert_alembic_to_rust_owned,
    create_new_sqlite_database,
    get_alembic_config,
    inspect_schema_state,
    upgrade_existing_sqlite_database,
)
from degenbot.degenbot_rs import DatabaseSchemaStale
from degenbot.exceptions.infrastructure import BackupExists
from degenbot.version import __version__


@cli.group()
def database() -> None:
    """Database commands."""


@database.command("backup")
@click.pass_obj
def database_backup(bot: Bot) -> None:
    """Back up the database.

    Raises:
        Abort: See function documentation.

    """
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
    """Remove and recreate the database.

    Raises:
        Abort: See function documentation.

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
    """Upgrade the database to the latest schema.

    Raises:
        Abort: See function documentation.

    """
    with bot.db() as _:
        current_version = MigrationContext.configure(
            connection=bot.db.connection(),
        ).get_current_revision()
    latest_version = ScriptDirectory.from_config(
        config=get_alembic_config(database_path=bot.config.database.path),
    ).get_current_head()

    if force or click.confirm(
        f"The database at {bot.config.database.path} will be upgraded from version {current_version} to {latest_version}. Do you want to proceed?",  # noqa:E501
        default=False,
    ):
        upgrade_existing_sqlite_database(database_path=bot.config.database.path)
    else:
        raise click.Abort


@database.command("compact")
@click.pass_obj
def database_compact(bot: Bot) -> None:
    """Compact the database."""
    compact_sqlite_database(bot.config.database.path)


@database.command("cutover")
@click.option(
    "--dry-run",
    is_flag=True,
    help="Report the schema state + what cutover would do; write nothing.",
)
@click.option(
    "--force",
    is_flag=True,
    help="Skip the confirmation prompt and run the cutover.",
)
@click.pass_obj
def database_cutover(bot: Bot, *, dry_run: bool, force: bool) -> None:
    """Flip an Alembic-stamped DB into Rust schema ownership (ADR-010).

    A one-way, opt-in cutover: drops the ``alembic_version`` table and stamps
    ``_degenbot_db_schema_version`` so future schema bumps are Rust-owned.
    Nothing on the open path auto-runs it. Use ``--dry-run`` first to inspect
    the current schema state without writing.

    Raises:
        Abort: See function documentation.
        SystemExit: On a stale / unrecognized / fresh DB when forcing.

    """
    database_path = bot.config.database.path
    state = inspect_schema_state(database_path=database_path)

    if dry_run:
        if state == "alembic_current":
            click.echo(
                f"Schema state: {state}. "
                "Would cutover from Alembic to Rust ownership (drop "
                "`alembic_version`, stamp `_degenbot_db_schema_version`).",
            )
        elif state == "rust_owned":
            click.echo(
                f"Schema state: {state}. "
                "Already Rust-owned — cutover is a no-op.",
            )
        elif state == "alembic_stale":
            click.echo(
                f"Schema state: {state}. "
                "Schema is stale — run `degenbot database upgrade` first.",
            )
        elif state == "fresh_standalone":
            click.echo(
                f"Schema state: {state}. "
                "No Alembic history — nothing to cutover.",
            )
        else:  # "unrecognized"
            click.echo(
                f"Schema state: {state}. "
                "Unrecognized database (foreign file).",
            )
        return

    if state == "alembic_stale":
        click.echo(
            "The database schema is stale; run `degenbot database upgrade` first.",
        )
        raise SystemExit(1)
    if state == "unrecognized":
        click.echo(
            "The database is unrecognized (a foreign SQLite file); cutover refused.",
        )
        raise SystemExit(1)
    if state == "fresh_standalone":
        click.echo(
            "The database has no Alembic history; there is nothing to cutover.",
        )
        raise SystemExit(1)

    # state is alembic_current or rust_owned.
    if not force and not click.confirm(
        f"The database at {database_path} will be cut over from Alembic to Rust "
        "schema ownership. This is ONE-WAY and cannot be undone. Proceed?",
        default=False,
    ):
        raise click.Abort

    try:
        outcome = convert_alembic_to_rust_owned(database_path=database_path)
    except DatabaseSchemaStale:
        click.echo(
            "The database schema is stale; run `degenbot database upgrade` first.",
        )
        raise SystemExit(1) from None

    if outcome == "converted":
        click.echo(
            f"Cut over database at {database_path} from Alembic to Rust "
            "ownership (the `alembic_version` table was dropped and "
            "`_degenbot_db_schema_version` was stamped).",
        )
    else:  # "already_rust_owned"
        click.echo(
            "The database is already Rust-owned; cutover was a no-op.",
        )
