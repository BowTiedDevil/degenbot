"""Aave V3 token processors for handling revision-specific contract logic."""

from degenbot.aave.processors.base import (
    BurnResult,
    CollateralBurnEvent,
    CollateralMintEvent,
    CollateralTokenProcessor,
    DebtBurnEvent,
    DebtMintEvent,
    DebtTokenProcessor,
    GhoBurnResult,
    GhoDebtTokenProcessor,
    GhoMintResult,
    MathLibraries,
    MintResult,
    PercentageMathLibrary,
    ProcessingResult,
    TokenProcessor,
    WadRayMathLibrary,
)
from degenbot.aave.processors.factory import TokenProcessorFactory

__all__ = [
    "BurnResult",
    "CollateralBurnEvent",
    "CollateralMintEvent",
    "CollateralTokenProcessor",
    "DebtBurnEvent",
    "DebtMintEvent",
    "DebtTokenProcessor",
    "GhoBurnResult",
    "GhoDebtTokenProcessor",
    "GhoMintResult",
    "MathLibraries",
    "MintResult",
    "PercentageMathLibrary",
    "ProcessingResult",
    "TokenProcessor",
    "TokenProcessorFactory",
    "WadRayMathLibrary",
]
