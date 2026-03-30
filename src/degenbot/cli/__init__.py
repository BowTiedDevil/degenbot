import atexit
import os
from pathlib import Path

import click

from degenbot.logging import logger

if os.environ.get("DEGENBOT_COVERAGE"):
    from coverage import Coverage


@click.group()
@click.version_option()
def cli() -> None: ...


# Initialize coverage tracking if DEGENBOT_COVERAGE envvar is set
_cov: Coverage | None = None
if os.environ.get("DEGENBOT_COVERAGE") and not os.environ.get("PYTEST_VERSION"):
    _cov = Coverage(
        config_file=False,
        include=["**/cli/**/*.py"],
        branch=True,
    )
    _cov.start()
    logger.info("Code coverage tracking enabled")

    @atexit.register
    def generate_coverage_report() -> None:
        """Generate coverage report on exit."""
        if _cov is not None:
            _cov.stop()
            output_dir = os.environ.get("DEGENBOT_COVERAGE_OUTPUT", "htmlcov")
            Path(output_dir).mkdir(parents=True, exist_ok=True)
            _cov.html_report(directory=output_dir)
            logger.info(f"Coverage report saved to {output_dir}")


from . import aave, database, exchange, pool  # noqa: F401, E402
