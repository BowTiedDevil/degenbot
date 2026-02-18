"""Collateral (aToken) processors."""

from degenbot.aave.processors.collateral.v1 import CollateralV1Processor
from degenbot.aave.processors.collateral.v3 import CollateralV3Processor
from degenbot.aave.processors.collateral.v4 import CollateralV4Processor
from degenbot.aave.processors.collateral.v5 import CollateralV5Processor

__all__ = [
    "CollateralV1Processor",
    "CollateralV3Processor",
    "CollateralV4Processor",
    "CollateralV5Processor",
]
