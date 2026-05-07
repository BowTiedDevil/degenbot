import click

from degenbot.bot import Bot


@click.group()
@click.version_option()
@click.pass_context
def cli(ctx: click.Context) -> None:
    ctx.ensure_object(dict)
    ctx.obj["bot"] = Bot.from_config_file()


from . import aave, database, exchange, pool  # noqa: F401, E402
