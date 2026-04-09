"""Constants and enums for Aave V3 CLI processing."""

from enum import Enum
from typing import Protocol

from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3OracleEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    AaveV3StkAaveEvent,
)
from degenbot.aave.operation_types import OperationType

# Module-level cache: topic -> category name for Aave events
AAVE_EVENT_TOPIC_TO_CATEGORY: dict[bytes, str] = {
    **{bytes(e.value): e.name for e in AaveV3PoolEvent},
    **{bytes(e.value): e.name for e in AaveV3StkAaveEvent},
    **{bytes(e.value): e.name for e in AaveV3ScaledTokenEvent},
    **{bytes(e.value): e.name for e in AaveV3GhoDebtTokenEvent},
    **{bytes(e.value): e.name for e in AaveV3PoolConfigEvent},
    **{bytes(e.value): e.name for e in AaveV3OracleEvent},
}

# Liquidation operation types (used to identify liquidation operations in multiple places)
LIQUIDATION_OPERATION_TYPES = {
    OperationType.LIQUIDATION,
    OperationType.GHO_LIQUIDATION,
}


class UserOperation(Enum):
    """User operation types for Aave V3 token events."""

    AAVE_REDEEM = "AAVE REDEEM"
    AAVE_STAKED = "AAVE STAKED"
    BORROW = "BORROW"
    DEPOSIT = "DEPOSIT"
    GHO_BORROW = "GHO BORROW"
    GHO_INTEREST_ACCRUAL = "GHO INTEREST ACCRUAL"
    GHO_REPAY = "GHO REPAY"
    REPAY = "REPAY"
    STKAAVE_TRANSFER = "stkAAVE TRANSFER"
    WITHDRAW = "WITHDRAW"


# Revision constants
GHO_DISCOUNT_DEPRECATION_REVISION = 4
SCALED_AMOUNT_POOL_REVISION = 9

# Display limit for position risk analysis output
POSITION_RISK_DISPLAY_LIMIT = 20


class WadRayMathLibrary(Protocol):
    """Protocol for WadRay math operations."""

    def ray_div(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...
