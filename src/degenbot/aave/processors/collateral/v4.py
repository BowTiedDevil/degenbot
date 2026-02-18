"""Collateral token processor for revision 4."""

import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV4Processor(CollateralV1Processor):
    """Processor for AToken revision 4."""

    revision = 4

    def __init__(self) -> None:
        """Initialize with v3.4 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_4.wad_ray_math,
            percentage=aave_library_v3_4.percentage_math,
        )
