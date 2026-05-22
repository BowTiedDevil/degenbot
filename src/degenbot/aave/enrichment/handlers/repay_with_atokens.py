"""REPAY_WITH_ATOKENS operation handler.

REPAY_WITH_ATOKENS is a composite operation that burns both aTokens (collateral)
and vTokens (debt). It has two special cases:

1. Collateral side: When interest exceeds the repayment amount on collateral,
   a COLLATERAL_MINT is emitted. Use COLLATERAL_BURN calculation (ceil rounding).

2. Debt side: When interest exceeds repayment on debt, a DEBT_MINT is emitted.
   Use DEBT_BURN calculation (floor rounding).

See WITHDRAW and REPAY handlers for detailed special case documentation.
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


class RepayWithAtokensHandler:
    """Handle enrichment for REPAY_WITH_ATOKENS operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.REPAY_WITH_ATOKENS}

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a REPAY_WITH_ATOKENS event.

        Handles both collateral and debt events with their respective
        special cases for interest exceeding repayment.
        """
        if operation.pool_event is None:
            msg = "REPAY_WITH_ATOKENS operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "REPAY_WITH_ATOKENS event has no index"
            raise EnrichmentError(msg)

        event_type = event.event_type

        # Handle collateral events
        if event_type in {
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.COLLATERAL_MINT,
        }:
            return self._handle_collateral_event(event, operation, context)

        # Handle debt events
        if event_type in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.DEBT_MINT,
        }:
            return self._handle_debt_event(event, operation, context)

        msg = f"REPAY_WITH_ATOKENS: unsupported event type {event_type}"
        raise EnrichmentError(msg)

    def _handle_collateral_event(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Handle collateral (aToken) side of REPAY_WITH_ATOKENS."""
        assert operation.pool_event is not None
        assert event.index is not None
        event_type = event.event_type

        # Check for interest > repayment on collateral (similar to WITHDRAW)
        if (
            event_type == ScaledTokenEventType.COLLATERAL_MINT
            and event.balance_increase is not None
            and event.amount < event.balance_increase
        ):
            logger.debug(
                "ENRICHMENT: REPAY_WITH_ATOKENS interest exceeds repayment on collateral - "
                "using COLLATERAL_BURN calculation (ceil rounding)"
            )

            # Extract actual repayment amount
            raw_amount = context.extract_pool_amount(
                pool_event=operation.pool_event,
                event_type=event_type,
                operation_type=operation.operation_type,
            )

            token_address = ChecksumAddress(event.event["address"])
            token_revision = context.get_token_revision(token_address)

            # Use COLLATERAL_BURN calculation (ceil)
            scaled_amount = context.calculate(
                event_type=ScaledTokenEventType.COLLATERAL_BURN,
                raw_amount=raw_amount,
                index=event.index,
                token_revision=token_revision,
            )

            # Skip validation for override
            return context.build_enriched_event(
                event=event,
                operation=operation,
                raw_amount=raw_amount,
                scaled_amount=None,
            )

        # Standard collateral burn
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event_type,
            operation_type=operation.operation_type,
        )

        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

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

    def _handle_debt_event(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Handle debt (vToken) side of REPAY_WITH_ATOKENS."""
        assert operation.pool_event is not None
        assert event.index is not None
        event_type = event.event_type

        # Check for interest > repayment on debt (similar to REPAY)
        if event_type == ScaledTokenEventType.DEBT_MINT and event.balance_increase is not None:
            logger.debug(
                "ENRICHMENT: REPAY_WITH_ATOKENS interest exceeds repayment on debt - "
                "using DEBT_BURN calculation (floor rounding)"
            )

            # Extract actual repayment amount
            raw_amount = context.extract_pool_amount(
                pool_event=operation.pool_event,
                event_type=event_type,
                operation_type=operation.operation_type,
            )

            token_address = ChecksumAddress(event.event["address"])
            token_revision = context.get_token_revision(token_address)

            # Use DEBT_BURN calculation (floor)
            scaled_amount = context.calculate(
                event_type=ScaledTokenEventType.DEBT_BURN,
                raw_amount=raw_amount,
                index=event.index,
                token_revision=token_revision,
            )

            # Do NOT skip validation - processing layer needs the value
            return context.build_enriched_event(
                event=event,
                operation=operation,
                raw_amount=raw_amount,
                scaled_amount=scaled_amount,
            )

        # Standard debt burn
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event_type,
            operation_type=operation.operation_type,
        )

        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        scaled_amount = context.calculate(
            event_type=ScaledTokenEventType.DEBT_BURN,
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
