"""REPAY/GHO_REPAY operation handler.

REPAY operations emit a DEBT_BURN event from the VariableDebtToken contract.
GHO_REPAY operations emit a GHO_DEBT_BURN event.
The Pool event contains the raw amount repaid by the user.

Special case: When interest exceeds repayment amount, the VariableDebtToken
emits a Mint event instead of a Burn event (VariableDebtToken _burnScaled).

Detection: In a REPAY/GHO_REPAY operation, if DEBT_MINT/GHO_DEBT_MINT is emitted
with balance_increase, it means interest > repayment.

In this case:
1. Extract the actual repayment amount from the Pool REPAY event
2. Use DEBT_BURN calculation (floor rounding) instead of DEBT_MINT

NOTE: Do NOT set scaled_amount=None for REPAY/GHO_REPAY with DEBT_MINT/GHO_DEBT_MINT.
The processing layer uses the enriched scaled_amount directly to avoid
1 wei rounding errors from deriving the amount from Mint event fields.
See debug/aave/0037 for details.
"""

from typing import TYPE_CHECKING, ClassVar

from eth_typing import ChecksumAddress

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class RepayHandler:
    """Handle enrichment for REPAY and GHO_REPAY operations."""

    operation_types: ClassVar[set[OperationType]] = {
        OperationType.REPAY,
        OperationType.GHO_REPAY,
    }

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a REPAY or GHO_REPAY event.

        Handles both standard burns and the interest>repayment Mint case.
        """
        if operation.pool_event is None:
            msg = "REPAY operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "REPAY event has no index"
            raise EnrichmentError(msg)

        event_type = event.event_type

        # Check for interest > repayment case
        if (
            event_type
            in {
                ScaledTokenEventType.DEBT_MINT,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }
            and event.balance_increase is not None
        ):
            return self._handle_interest_exceeds_repayment(
                event=event,
                operation=operation,
                context=context,
            )

        # Standard burn handling
        return self._handle_standard_burn(
            event=event,
            operation=operation,
            context=context,
        )

    def _handle_interest_exceeds_repayment(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Handle the case where interest exceeds repayment amount.

        The Mint event's amount represents the net debt increase (interest - repayment).
        Extract the actual repayment amount from the Pool event and use
        DEBT_BURN calculation (floor rounding).

        NOTE: Do NOT set scaled_amount=None. The processing layer uses
        the enriched scaled_amount directly.
        See debug/aave/0037 for details.
        """
        assert operation.pool_event is not None
        assert event.index is not None
        logger.debug(
            "ENRICHMENT: Interest exceeds repayment - using repay amount "
            "from pool event for burn calculation"
        )

        # Extract actual repayment amount from Pool event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        logger.debug(
            "ENRICHMENT: Interest exceeds repayment - using DEBT_BURN calculation (floor rounding)"
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Use DEBT_BURN calculation (floor rounding)
        # Note: DEBT_BURN works for both DEBT_MINT and GHO_DEBT_MINT
        scaled_amount = context.calculate(
            event_type=ScaledTokenEventType.DEBT_BURN,
            raw_amount=raw_amount,
            index=event.index,
            token_revision=token_revision,
        )

        # Do NOT set scaled_amount=None - the processing layer needs it
        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )

    def _handle_standard_burn(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Handle standard REPAY/GHO_REPAY burn event."""
        assert operation.pool_event is not None
        assert event.index is not None
        # Extract raw amount from Pool event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Determine calculation type based on event type
        if event.event_type == ScaledTokenEventType.GHO_DEBT_BURN:
            calc_event_type = ScaledTokenEventType.GHO_DEBT_BURN
        else:
            calc_event_type = ScaledTokenEventType.DEBT_BURN

        # Calculate scaled amount using debt burn (floor) rounding
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
