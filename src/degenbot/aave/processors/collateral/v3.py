"""Collateral token processor for revision 3."""

import degenbot.aave.libraries
from degenbot.aave.processors.base import MathLibraries
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV3Processor(CollateralV1Processor):
    """Processor for AToken revision 3."""

    revision = 3

    def __init__(self) -> None:
        self._math_libs = MathLibraries(
            wad_ray=degenbot.aave.libraries.wad_ray_math,
            percentage=degenbot.aave.libraries.percentage_math,
        )
