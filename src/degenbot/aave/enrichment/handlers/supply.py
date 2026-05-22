"""
SUPPLY operation handler.

SUPPLY operations emit a COLLATERAL_MINT event from the aToken contract.
The Pool event contains the raw amount supplied by the user.
"""

from typing import TYPE_CHECKING, ClassVar

from eth_typing import ChecksumAddress

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class SupplyHandler:
    """Handle enrichment for SUPPLY operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.SUPPLY}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a SUPPLY event.

        For SUPPLY operations:
        1. Extract raw amount from the Pool SUPPLY event
        2. Calculate scaled amount using collateral mint (floor) rounding
        """
        if operation.pool_event is None:
            msg = "SUPPLY operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "SUPPLY event has no index"
            raise EnrichmentError(msg)

        # Extract raw amount from Pool event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Calculate scaled amount using collateral mint (floor) rounding
        scaled_amount = context.calculate(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            raw_amount=raw_amount,
            index=event.index,
            token_revision=token_revision,
        )

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )
