"""DEFICIT_COVERAGE operation handler.

Umbrella deficit coverage involves a BalanceTransfer event followed by a
Burn event pair. This operation type handles both event types:

1. COLLATERAL_TRANSFER: Internal transfer, raw_amount = scaled_amount
2. COLLATERAL_BURN: Standard burn calculation with ceil rounding

No special handling required - both are standard operations.
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


class DeficitCoverageHandler:
    """Handle enrichment for DEFICIT_COVERAGE operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.DEFICIT_COVERAGE}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Enrich a DEFICIT_COVERAGE event.

        Handles both COLLATERAL_TRANSFER and COLLATERAL_BURN events.
        - Transfers: raw_amount = scaled_amount (no index scaling)
        - Burns: standard collateral burn calculation

        Returns:
            The computed value.

        Raises:
                     EnrichmentError: If the operation fails.

        """
        event_type = event.event_type

        # Handle transfer events (no index-based scaling)
        if event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            raw_amount = event.amount
            scaled_amount = event.amount  # No scaling for transfers

            return context.build_enriched_event(
                event=event,
                operation=operation,
                raw_amount=raw_amount,
                scaled_amount=scaled_amount,
            )

        # Handle burn events (standard calculation)
        if event_type == ScaledTokenEventType.COLLATERAL_BURN:
            if operation.pool_event is None:
                msg = "DEFICIT_COVERAGE burn has no pool event"
                raise EnrichmentError(msg)

            if event.index is None:
                msg = "DEFICIT_COVERAGE burn has no index"
                raise EnrichmentError(msg)

            # Extract raw amount from Pool event
            raw_amount = context.extract_pool_amount(
                pool_event=operation.pool_event,
                event_type=event_type,
                operation_type=operation.operation_type,
            )

            # Get token revision for calculation
            token_address = ChecksumAddress(event.event["address"])
            token_revision = context.get_token_revision(token_address)

            # Calculate scaled amount using collateral burn (ceil) rounding
            scaled_amount = context.calculate(
                event_type=ScaledTokenEventType.COLLATERAL_BURN,
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

        msg = f"DEFICIT_COVERAGE: unsupported event type {event_type}"
        raise EnrichmentError(msg)
