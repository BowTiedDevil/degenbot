"""GHO variable debt token processors."""

from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor
from degenbot.aave.processors.debt.gho.v2 import GhoV2Processor
from degenbot.aave.processors.debt.gho.v4 import GhoV4Processor

__all__ = [
    "GhoV1Processor",
    "GhoV2Processor",
    "GhoV4Processor",
]
