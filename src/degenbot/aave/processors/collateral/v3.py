"""Collateral token processor for revision 3."""

import degenbot.aave.libraries as aave_library
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV3Processor(CollateralV1Processor):
    """Processor for AToken revision 3."""

    revision = 3
    math_lib_version = "v3.3"

    def __init__(self) -> None:
        """Initialize with v3.3 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library.wad_ray_math,
            percentage=aave_library.percentage_math,
        )
