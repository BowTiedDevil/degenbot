"""Debt (vToken) processors."""

from degenbot.aave.processors.debt.v1 import DebtV1Processor
from degenbot.aave.processors.debt.v3 import DebtV3Processor
from degenbot.aave.processors.debt.v4 import DebtV4Processor
from degenbot.aave.processors.debt.v5 import DebtV5Processor

__all__ = [
    "DebtV1Processor",
    "DebtV3Processor",
    "DebtV4Processor",
    "DebtV5Processor",
]
