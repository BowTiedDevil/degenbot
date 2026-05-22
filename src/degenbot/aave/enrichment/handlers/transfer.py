"""BALANCE_TRANSFER operation handler.

Balance transfers are internal transfers between users that don't involve
Pool events. For transfers, raw_amount = scaled_amount (no index-based
calculation) because the amount is already in scaled units.
"""

from typing import TYPE_CHECKING, ClassVar

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class BalanceTransferHandler:
    """Handle enrichment for BALANCE_TRANSFER operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.BALANCE_TRANSFER}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Enrich a BALANCE_TRANSFER event.

        Transfers bypass index-based scaling entirely - the amount is already
        in scaled units. raw_amount = scaled_amount.

        Returns:
            The computed value.

        """
        # Transfers have no pool event and use amount directly
        raw_amount = event.amount
        scaled_amount = event.amount  # No scaling for transfers

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )
