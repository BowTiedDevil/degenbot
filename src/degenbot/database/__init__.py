from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory

from degenbot.config import settings
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.logging import logger
from degenbot.version import __version__

db_session = get_scoped_sqlite_session(database_path=settings.database.path)

with db_session() as session:
    current_database_version = MigrationContext.configure(
        connection=db_session.connection()
    ).get_current_revision()
latest_database_version = ScriptDirectory.from_config(
    config=get_alembic_config()
).get_current_head()


if current_database_version is not None and current_database_version != latest_database_version:
    logger.warning(
        f"The current database revision ({current_database_version}) does not match the latest "
        f"({latest_database_version}) for {__package__} version {__version__}!"
        "\n"
        "Database-related features may raise exceptions if you continue. Perform database "
        "migrations with 'degenbot database upgrade'."
    )
