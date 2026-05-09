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
from degenbot.aave.processors.processor import (
    UnifiedCollateralProcessor,
    UnifiedDebtProcessor,
    UnifiedGhoProcessor,
)
from degenbot.aave.processors.strategies import (
    COLLATERAL_STRATEGIES,
    DEBT_STRATEGIES,
    GHO_DISCOUNT_STRATEGIES,
    GHO_STRATEGIES,
    DiscountStrategy,
    RoundingMode,
    RoundingStrategy,
)

__all__ = [
    # Strategies
    "COLLATERAL_STRATEGIES",
    "DEBT_STRATEGIES",
    "GHO_DISCOUNT_STRATEGIES",
    "GHO_STRATEGIES",
    # Event dataclasses
    "CollateralBurnEvent",
    "CollateralMintEvent",
    # Protocols
    "CollateralTokenProcessor",
    "DebtBurnEvent",
    "DebtMintEvent",
    "DebtTokenProcessor",
    "DiscountStrategy",
    "GhoDebtTokenProcessor",
    # Result dataclasses
    "GhoScaledTokenBurnResult",
    "GhoScaledTokenMintResult",
    # Enums and types
    "GhoUserOperation",
    "MathLibraries",
    "PercentageMathLibrary",
    "ProcessingResult",
    "RoundingMode",
    "RoundingStrategy",
    "ScaledTokenBurnResult",
    "ScaledTokenMintResult",
    "TokenProcessor",
    # Factory
    "TokenProcessorFactory",
    # Unified processors
    "UnifiedCollateralProcessor",
    "UnifiedDebtProcessor",
    "UnifiedGhoProcessor",
    "WadRayMathLibrary",
]
