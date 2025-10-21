from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory

from degenbot.config import get_alembic_config, settings
from degenbot.logging import logger
from degenbot.version import __version__

from . import operations

default_db_session = operations.get_scoped_sqlite_session(database_path=settings.database.path)
current_database_version = MigrationContext.configure(
    default_db_session.connection()
).get_current_revision()
latest_database_version = ScriptDirectory.from_config(get_alembic_config()).get_current_head()

if current_database_version is not None and current_database_version != latest_database_version:
    logger.warning(
        f"The current database revision ({current_database_version}) does not match the latest "
        f"({latest_database_version}) for {__package__} version {__version__}!"
        "\n"
        "Database-related features may raise exceptions if you continue! Perform database "
        "migrations with 'degenbot database upgrade'."
    )
