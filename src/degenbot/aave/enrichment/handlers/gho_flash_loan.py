"""GHO_FLASH_LOAN operation handler.

GHO flash loan deficit coverage. When a GHO flash loan creates a deficit,
the Pool emits a DEFICIT_CREATED event which triggers a GHO_DEBT_BURN.

Standard debt burn calculation with floor rounding.
"""

from typing import TYPE_CHECKING, ClassVar

from eth_typing import ChecksumAddress

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class GhoFlashLoanHandler:
    """Handle enrichment for GHO_FLASH_LOAN operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.GHO_FLASH_LOAN}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a GHO_FLASH_LOAN event.

        GHO flash loan deficits emit GHO_DEBT_BURN events.
        Standard burn calculation using floor rounding.
        """
        if operation.pool_event is None:
            msg = "GHO_FLASH_LOAN operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "GHO_FLASH_LOAN event has no index"
            raise EnrichmentError(msg)

        # Extract raw amount from Pool DEFICIT_CREATED event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Calculate scaled amount using GHO debt burn (floor) rounding
        scaled_amount = context.calculate(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
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
