"""BORROW operation handler.

BORROW operations emit a DEBT_MINT event from the VariableDebtToken contract.
GHO_BORROW operations emit a GHO_DEBT_MINT event.
The Pool event contains the raw amount borrowed by the user.
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


class BorrowHandler:
    """Handle enrichment for BORROW and GHO_BORROW operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.BORROW, OperationType.GHO_BORROW}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Enrich a BORROW or GHO_BORROW event.

        For BORROW operations:
        1. Extract raw amount from the Pool BORROW event
        2. Calculate scaled amount using debt mint (ceil) rounding
        """
        if operation.pool_event is None:
            msg = "BORROW operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "BORROW event has no index"
            raise EnrichmentError(msg)

        # Determine event type for calculation
        if operation.operation_type == OperationType.GHO_BORROW:
            calc_event_type = ScaledTokenEventType.GHO_DEBT_MINT
        else:
            calc_event_type = ScaledTokenEventType.DEBT_MINT

        # Extract raw amount from Pool event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Calculate scaled amount using debt mint (ceil) rounding
        scaled_amount = context.calculate(
            event_type=calc_event_type,
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
