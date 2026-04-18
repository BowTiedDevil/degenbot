import atexit
import os
from pathlib import Path

import click

from degenbot.logging import logger


@click.group()
@click.version_option()
def cli() -> None: ...


# Initialize coverage tracking if DEGENBOT_COVERAGE envvar is set
if os.environ.get("DEGENBOT_COVERAGE") and not os.environ.get("PYTEST_VERSION"):
    from coverage import Coverage

    # Remove stale/corrupted .coverage file so Coverage doesn't try to
    # re-use an invalid SQLite database. auto_data=True appends to an
    # existing file, so a broken one must be cleared first.
    _cov_data_file = ".coverage"
    _cov_path = Path(_cov_data_file)
    if _cov_path.exists():
        _remove = False
        if _cov_path.stat().st_size == 0:
            _remove = True
            logger.warning("Removing empty .coverage file")
        else:
            # Validate the SQLite database has the expected Coverage schema.
            # Coverage's _read_db only catches a missing coverage_schema
            # table, but a partially-written DB can have that table yet
            # lack others (meta, file), causing "no such table" crashes
            # during auto_data load.
            import sqlite3

            try:
                conn = sqlite3.connect(str(_cov_path))
                cursor = conn.execute(
                    "SELECT name FROM sqlite_master "
                    "WHERE type='table' AND name IN "
                    "('coverage_schema','meta','file')"
                )
                tables = {row[0] for row in cursor.fetchall()}
                conn.close()
                required_tables = {"coverage_schema", "meta", "file"}
                if not required_tables.issubset(tables):
                    _remove = True
                    missing = required_tables - tables
                    logger.warning(
                        f"Removing corrupted .coverage file "
                        f"(missing tables: {', '.join(sorted(missing))})"
                    )
            except sqlite3.DatabaseError as e:
                _remove = True
                logger.warning(
                    f"Removing corrupted .coverage file (SQLite error: {e})"
                )
        if _remove:
            _cov_path.unlink()

    _cov = Coverage(
        config_file=False,
        data_file=_cov_data_file,
        include=["**/cli/**/*.py"],
        branch=True,
        auto_data=True,
    )
    _cov.start()
    logger.info("Code coverage tracking enabled")

    @atexit.register
    def generate_coverage_report() -> None:
        """
        Generate coverage report on exit.
        """

        try:
            _cov.stop()
            output_dir = os.environ.get("DEGENBOT_COVERAGE_OUTPUT", "htmlcov")
            Path(output_dir).mkdir(parents=True, exist_ok=True)
            _cov.html_report(directory=output_dir)
            logger.info(f"Coverage report saved to {output_dir}")
        except Exception as e:
            logger.error(f"Failed to generate coverage report: {e}")


from . import aave, database, exchange, pool  # noqa: F401, E402
