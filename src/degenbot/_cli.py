from textwrap import dedent
from typing import Literal

import click
import tomlkit
from pydantic import TypeAdapter

from degenbot import __version__
from degenbot.database import create_new_sqlite_database

from ._config import settings


@click.group()
@click.version_option()
def cli() -> None: ...


@cli.group()
def config() -> None: ...


@config.command("show")
@click.option(
    "--json",
    "output_format",
    flag_value="json",
    type=str,
    help="Show configuration in JSON format",
)
@click.option(
    "--toml",
    "output_format",
    flag_value="toml",
    type=str,
    help="Show configuration in TOML format (default)",
    default=True,
)
def config_show(output_format: Literal["json", "toml"]) -> None:
    match output_format:
        case "json":
            click.echo(
                TypeAdapter(dict).dump_json(
                    settings.model_dump(),
                    indent=2,
                ),
            )
        case "toml":
            click.echo(
                tomlkit.dumps(
                    settings.model_dump(),
                ),
            )


@cli.group()
def database() -> None: ...


@database.command("reset")
def database_reset() -> None:
    user_confirm = click.confirm(
        dedent(
            f"""\
            The existing DB at {settings.database.path} will be removed and a new, empty DB will be created and initialized using the schema included in {__package__} version {__version__}.
            Do you want to proceed?"""  # noqa: E501
        ),
        default=False,
    )
    if user_confirm:
        create_new_sqlite_database(settings.database.path)
    else:
        raise click.Abort()



@database.command("verify")
def database_verify() -> None:
    click.echo(f"(placeholder) DB at {settings.database.path} verified")
