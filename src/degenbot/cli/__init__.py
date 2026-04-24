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

    _cov = Coverage(
        config_file=False,
        include=[
            "**/aave/**/*.py",
            "**/cli/**/*.py",
        ],
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

        _cov.stop()
        _cov.save()
        output_dir = os.environ.get("DEGENBOT_COVERAGE_OUTPUT", "htmlcov")
        Path(output_dir).mkdir(parents=True, exist_ok=True)
        _cov.html_report(directory=output_dir)
        logger.info(f"Coverage report saved to {output_dir}")


from . import aave, database, exchange, pool  # noqa: F401, E402
