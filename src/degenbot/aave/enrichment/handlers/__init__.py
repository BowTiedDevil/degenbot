"""Operation handlers for Aave event enrichment."""

from typing import TYPE_CHECKING, cast

from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.borrow import BorrowHandler
from degenbot.aave.enrichment.handlers.deficit_coverage import DeficitCoverageHandler
from degenbot.aave.enrichment.handlers.gho_flash_loan import GhoFlashLoanHandler
from degenbot.aave.enrichment.handlers.interest_accrual import InterestAccrualHandler
from degenbot.aave.enrichment.handlers.liquidation import LiquidationHandler
from degenbot.aave.enrichment.handlers.mint_to_treasury import MintToTreasuryHandler
from degenbot.aave.enrichment.handlers.repay import RepayHandler
from degenbot.aave.enrichment.handlers.repay_with_atokens import RepayWithAtokensHandler
from degenbot.aave.enrichment.handlers.stkaave_transfer import StkAaveTransferHandler
from degenbot.aave.enrichment.handlers.supply import SupplyHandler
from degenbot.aave.enrichment.handlers.transfer import BalanceTransferHandler
from degenbot.aave.enrichment.handlers.unknown import UnknownHandler
from degenbot.aave.enrichment.handlers.withdraw import WithdrawHandler
from degenbot.aave.operation_types import OperationType

# Registry mapping OperationType to handler instances
HANDLER_REGISTRY: dict["OperationType", OperationHandler] = {}


def _register_handlers() -> None:
    for handler_class in [
        InterestAccrualHandler,
        MintToTreasuryHandler,
        BalanceTransferHandler,
        SupplyHandler,
        BorrowHandler,
        StkAaveTransferHandler,
        UnknownHandler,
        GhoFlashLoanHandler,
        DeficitCoverageHandler,
        WithdrawHandler,
        RepayHandler,
        RepayWithAtokensHandler,
        LiquidationHandler,
    ]:
        handler = cast("OperationHandler", handler_class())
        for op_type in handler.operation_types:
            HANDLER_REGISTRY[op_type] = handler


_register_handlers()

__all__ = [
    "HANDLER_REGISTRY",
    "BalanceTransferHandler",
    "BorrowHandler",
    "DeficitCoverageHandler",
    "GhoFlashLoanHandler",
    "InterestAccrualHandler",
    "LiquidationHandler",
    "MintToTreasuryHandler",
    "OperationHandler",
    "RepayHandler",
    "RepayWithAtokensHandler",
    "StkAaveTransferHandler",
    "SupplyHandler",
    "UnknownHandler",
    "WithdrawHandler",
]
