"""Aave V3 position analysis for liquidation risk monitoring.

Legacy location — re-exports from the analysis package.
New code should import from `degenbot.aave.analysis`.
"""

from degenbot.aave.analysis.core import (  # noqa: F401
    BASIS_POINTS,
    CollateralPositionData,
    DebtPositionData,
    HEALTH_FACTOR_AT_RISK_THRESHOLD,
    HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD,
    PositionAnalysisResult,
    UserPositionSummary,
    calculate_actual_collateral_balance,
    calculate_actual_debt_balance,
    calculate_health_factor,
)
from degenbot.aave.analysis.orchestrator import (  # noqa: F401
    DatabasePositionQuery,
    OraclePriceFetcher,
    PositionAnalysisService,
    analyze_positions_for_market,
)

__all__ = [
    "BASIS_POINTS",
    "HEALTH_FACTOR_AT_RISK_THRESHOLD",
    "HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD",
    "CollateralPositionData",
    "DebtPositionData",
    "PositionAnalysisResult",
    "UserPositionSummary",
    "calculate_actual_collateral_balance",
    "calculate_actual_debt_balance",
    "calculate_health_factor",
    "analyze_positions_for_market",
    "DatabasePositionQuery",
    "OraclePriceFetcher",
    "PositionAnalysisService",
]
