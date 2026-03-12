"""Debt token processor for revision 3."""

import degenbot.aave.libraries
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.debt.v1 import DebtV1Processor


class DebtV3Processor(DebtV1Processor):
    """Processor for VToken revision 3."""

    revision = 3

    def __init__(self) -> None:
        self._math_libs = MathLibraries(
            wad_ray=degenbot.aave.libraries.wad_ray_math,
            percentage=degenbot.aave.libraries.percentage_math,
        )
