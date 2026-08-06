"""CLI commands for degenbot operations."""

import click

from degenbot.bot import Bot
from degenbot.exceptions import DatabaseSchemaStale


class DegenbotCLI(click.Group):
    """The degenbot CLI root group.

    Catches :class:`DatabaseSchemaStale` (raised by the degenbot-db PyO3 seam
    when the DB is stamped at a prior Alembic revision) and prints a friendly
    one-line remediation hint instead of a Python traceback. End users see
    "run `degenbot database upgrade`", not a wall of stack frames.

    The ``database upgrade`` command itself is unaffected: its
    :func:`upgrade_existing_sqlite_database` shell catches
    :class:`DatabaseSchemaStale` first and runs the Alembic migration. This
    outer catch is the safety net for every *other* subcommand that trips a
    stale DB mid-operation.
    """

    def invoke(self, ctx: click.Context) -> object:
        """Run the CLI, translating stale-DB errors into a friendly hint.

        Returns:
            The result of delegating ``invoke`` to the parent group, unless a
            stale-DB error is caught (then ``ctx.exit(1)`` terminates).

        """
        try:
            return super().invoke(ctx)
        except DatabaseSchemaStale as exc:
            click.echo(str(exc), err=True)
            ctx.exit(1)


@click.group(cls=DegenbotCLI)
@click.version_option()
@click.pass_context
def cli(ctx: click.Context) -> None:
    """Perform cli."""
    ctx.obj = Bot.from_config_file()


from . import (  # ruff:ignore[unused-import, module-import-not-at-top-of-file]
    aave,
    database,
    exchange,
    path,
    pool,
)
