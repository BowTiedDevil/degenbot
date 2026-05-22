"""
STKAAVE_TRANSFER operation handler.

stkAAVE is the GHO discount token. It's a standard ERC20 token, so transfers
don't use index-based scaling. raw_amount = scaled_amount.
"""

from typing import TYPE_CHECKING, ClassVar

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class StkAaveTransferHandler:
    """Handle enrichment for STKAAVE_TRANSFER operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.STKAAVE_TRANSFER}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a STKAAVE_TRANSFER event.

        stkAAVE is an ERC20 token, so transfers bypass index-based scaling.
        raw_amount = scaled_amount.
        """
        # stkAAVE transfers have no pool event and use amount directly
        raw_amount = event.amount
        scaled_amount = event.amount  # No scaling for ERC20 transfers

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )
