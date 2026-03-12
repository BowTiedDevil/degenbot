"""Aave V3 token processors for handling revision-specific contract logic."""

from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    CollateralTokenProcessor,
    DebtBurnEvent,
    DebtMintEvent,
    DebtTokenProcessor,
    GhoDebtTokenProcessor,
    GhoScaledTokenBurnResult,
    GhoScaledTokenMintResult,
    GhoUserOperation,
    MathLibraries,
    PercentageMathLibrary,
    ProcessingResult,
    ScaledTokenBurnResult,
    ScaledTokenMintResult,
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
    "GhoDebtTokenProcessor",
    "GhoScaledTokenBurnResult",
    "GhoScaledTokenMintResult",
    "GhoUserOperation",
    "MathLibraries",
    "PercentageMathLibrary",
    "ProcessingResult",
    "ScaledTokenBurnResult",
    "ScaledTokenMintResult",
    "TokenProcessor",
    "TokenProcessorFactory",
    "WadRayMathLibrary",
]
