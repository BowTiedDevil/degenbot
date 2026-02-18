"""Aave V3 token processors for handling revision-specific contract logic."""

from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    CollateralTokenProcessor,
    DebtBurnEvent,
    DebtMintEvent,
    DebtTokenProcessor,
    GhoTokenProcessor,
    MathLibraries,
    PercentageMathLibrary,
    TokenProcessor,
    WadRayMathLibrary,
)
from degenbot.aave.processors.factory import TokenProcessorFactory

__all__ = [
    "CollateralBurnEvent",
    "CollateralMintEvent",
    "CollateralTokenProcessor",
    "DebtBurnEvent",
    "DebtMintEvent",
    "DebtTokenProcessor",
    "GhoTokenProcessor",
    "MathLibraries",
    "PercentageMathLibrary",
    "TokenProcessor",
    "TokenProcessorFactory",
    "WadRayMathLibrary",
]
