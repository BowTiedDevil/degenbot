import click


@click.group()
@click.version_option()
def cli() -> None: ...


from . import database, exchange, pool  # noqa: F401, E402
