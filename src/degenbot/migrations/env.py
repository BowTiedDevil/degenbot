"""Alembic migration environment configuration."""
import logging.config
import os

from alembic import context
from sqlalchemy import engine_from_config, pool

from degenbot.database.models import Base
from degenbot.database.operations import _get_sqlite_db_string

# this is the Alembic Config object, which provides
# access to the values within the .ini file in use.
config = context.config

# Database path: requires explicit DEGENBOT_DATABASE_PATH env var
database_path = os.environ.get("DEGENBOT_DATABASE_PATH")
if database_path:
    config.set_main_option(
        "sqlalchemy.url",
        f"sqlite:///{database_path}",
    )
else:
    from degenbot.config import _init_config

    degenbot_config = _init_config()
    config.set_main_option(
        "sqlalchemy.url",
        f"sqlite:///{_get_sqlite_db_string(degenbot_config.database.path)}",
    )


# Interpret the config file for Python logging.
# This line sets up loggers.
if config.config_file_name is not None:
    logging.config.fileConfig(config.config_file_name)

# add your model's MetaData object here
# for 'autogenerate' support
target_metadata = Base.metadata


def run_migrations_offline() -> None:
    """
    Run migrations in 'offline' mode.

    This configures the context with just a URL
    and not an Engine, though an Engine is acceptable
    here as well.  By skipping the Engine creation
    we don't even need a DBAPI to be available.

    Calls to context.execute() here emit the given string to the
    script output.

    """
    url = config.get_main_option("sqlalchemy.url")
    context.configure(
        url=url,
        target_metadata=target_metadata,
        literal_binds=True,
        dialect_opts={"paramstyle": "named"},
    )

    with context.begin_transaction():
        context.run_migrations()


def run_migrations_online() -> None:
    """
    Run migrations in 'online' mode.

    In this scenario we need to create an Engine
    and associate a connection with the context.

    """
    connectable = engine_from_config(
        config.get_section(config.config_ini_section, {}),
        prefix="sqlalchemy.",
        poolclass=pool.NullPool,
    )

    with connectable.connect() as connection:
        context.configure(
            connection=connection,
            target_metadata=target_metadata,
            render_as_batch=True,
        )

        with context.begin_transaction():
            context.run_migrations()


if context.is_offline_mode():
    run_migrations_offline()
else:
    run_migrations_online()
