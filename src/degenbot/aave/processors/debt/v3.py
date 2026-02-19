"""Debt token processor for revision 3."""

import degenbot.aave.libraries.v3_3 as aave_library_v3_3
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.debt.v1 import DebtV1Processor


class DebtV3Processor(DebtV1Processor):
    """Processor for VToken revision 3."""

    revision = 3
    math_lib_version = "v3.3"

    def __init__(self) -> None:
        """Initialize with v3.3 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_3.wad_ray_math,
            percentage=aave_library_v3_3.percentage_math,
        )
