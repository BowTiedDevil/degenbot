from enum import Enum, auto


class OperationType(Enum):
    """Types of Aave operations based on asset flows."""

    # Standard operations
    SUPPLY = auto()  # SUPPLY -> COLLATERAL_MINT
    WITHDRAW = auto()  # WITHDRAW -> COLLATERAL_BURN
    BORROW = auto()  # BORROW -> DEBT_MINT
    REPAY = auto()  # REPAY -> DEBT_BURN

    # Composite operations
    REPAY_WITH_ATOKENS = auto()  # REPAY -> DEBT_BURN + COLLATERAL_BURN
    LIQUIDATION = auto()  # LIQUIDATION_CALL -> DEBT_BURN + COLLATERAL_BURN

    # GHO-specific operations
    GHO_BORROW = auto()  # BORROW -> GHO_DEBT_MINT
    GHO_REPAY = auto()  # REPAY -> GHO_DEBT_BURN
    GHO_LIQUIDATION = auto()  # LIQUIDATION_CALL -> GHO_DEBT_BURN + COLLATERAL_BURN
    GHO_FLASH_LOAN = auto()  # DEFICIT_CREATED -> GHO_DEBT_BURN

    # Standalone events
    INTEREST_ACCRUAL = auto()  # Mint/Burn with no pool event
    BALANCE_TRANSFER = auto()  # Standalone BalanceTransfer
    DEFICIT_COVERAGE = auto()  # BalanceTransfer + Burn pair (Umbrella deficit coverage)
    MINT_TO_TREASURY = auto()  # Pool minting aTokens to treasury (no SUPPLY event)
    STKAAVE_TRANSFER = auto()  # stkAAVE (GHO Discount Token) transfer
    UNKNOWN = auto()
