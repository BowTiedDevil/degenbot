"""MINT_TO_TREASURY operation handler.

MINT_TO_TREASURY events are emitted when the Pool mints aTokens to the treasury
for accumulated interest.

Special case: MINT_TO_TREASURY requires position data (current balance and
last_index) to correctly calculate accruedToTreasury. The enrichment layer
doesn't have access to position data, so leave scaled_amount as None.
The correct calculation is performed in aave.py with position context.

See debug/aave/0014 - MINT_TO_TREASURY AccruedToTreasury Calculation Error.md
"""

from typing import TYPE_CHECKING, ClassVar

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class MintToTreasuryHandler:
    """Handle enrichment for MINT_TO_TREASURY operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.MINT_TO_TREASURY}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a MINT_TO_TREASURY event.

        MINT_TO_TREASURY events don't have a corresponding Pool event, and
        the scaled amount cannot be calculated without position context.
        The enrichment layer passes through the raw amount and leaves
        scaled_amount as None for later calculation.
        """
        raw_amount = event.amount
        scaled_amount = None  # Cannot calculate without position data

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )
