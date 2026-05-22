"""Constants and enums for Aave V3 CLI processing."""

from enum import Enum

from degenbot.aave.operation_types import OperationType

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

# Display limit for position risk analysis output
POSITION_RISK_DISPLAY_LIMIT = 20
