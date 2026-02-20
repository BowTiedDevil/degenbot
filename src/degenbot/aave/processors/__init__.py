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
    GhoUserOperation,
    MathLibraries,
    MintResult,
    PercentageMathLibrary,
    ProcessingResult,
    TokenProcessor,
    WadRayMathLibrary,
)
from degenbot.aave.processors.factory import TokenProcessorFactory
from degenbot.aave.processors.pool import PoolProcessor, PoolProcessorFactory

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
    "GhoUserOperation",
    "MathLibraries",
    "MintResult",
    "PercentageMathLibrary",
    "PoolProcessor",
    "PoolProcessorFactory",
    "ProcessingResult",
    "TokenProcessor",
    "TokenProcessorFactory",
    "WadRayMathLibrary",
]
