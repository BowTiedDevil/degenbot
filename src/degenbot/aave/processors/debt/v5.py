"""Debt token processor for revision 5."""

import degenbot.aave.libraries.v3_5 as aave_library_v3_5
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.debt.v1 import DebtV1Processor


class DebtV5Processor(DebtV1Processor):
    """Processor for VToken revision 5."""

    revision = 5

    def __init__(self) -> None:
        """Initialize with v3.5 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_5.wad_ray_math,
            percentage=aave_library_v3_5.percentage_math,
        )
