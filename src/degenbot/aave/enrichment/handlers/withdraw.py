"""WITHDRAW operation handler.

WITHDRAW operations emit a COLLATERAL_BURN event from the aToken contract.
The Pool event contains the raw amount withdrawn by the user.

Special case: When interest exceeds withdrawal amount, the aToken contract
emits a Mint event instead of a Burn event (AToken rev_4.sol:2836-2839).

Detection: In a WITHDRAW operation, if COLLATERAL_MINT has
amount < balance_increase, it means interest > withdrawal.

In this case:
1. Extract the actual withdrawal amount from the Pool WITHDRAW event
2. Use COLLATERAL_BURN calculation (ceil rounding) instead of COLLATERAL_MINT
3. Set scaled_amount=None to skip validation
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


class WithdrawHandler:
    """Handle enrichment for WITHDRAW operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.WITHDRAW}

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Enrich a WITHDRAW event.

        Handles both standard burns and the interest>withdrawal Mint case.

        Returns:
            The computed value.

        Raises:
                     EnrichmentError: If the operation fails.

        """
        if operation.pool_event is None:
            msg = "WITHDRAW operation has no pool event"
            raise EnrichmentError(msg)

        if event.index is None:
            msg = "WITHDRAW event has no index"
            raise EnrichmentError(msg)

        event_type = event.event_type

        # Check for interest > withdrawal case
        if (
            event_type == ScaledTokenEventType.COLLATERAL_MINT
            and event.balance_increase is not None
            and event.amount < event.balance_increase
        ):
            return self._handle_interest_exceeds_withdrawal(
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

    def _handle_interest_exceeds_withdrawal(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Handle the case where interest exceeds withdrawal amount.

        The Mint event's amount represents the net interest (interest - withdrawal),
        not the actual withdrawal. Extract the actual withdrawal amount from the
        Pool event and use COLLATERAL_BURN calculation (ceil rounding).

        Returns:
            The computed value.

        """
        assert operation.pool_event is not None
        assert event.index is not None
        logger.debug(
            "ENRICHMENT: Interest exceeds withdrawal - using withdraw amount "
            "from pool event for burn calculation",
        )

        # Extract actual withdrawal amount from Pool event
        raw_amount = context.extract_pool_amount(
            pool_event=operation.pool_event,
            event_type=event.event_type,
            operation_type=operation.operation_type,
        )

        logger.debug(
            "ENRICHMENT: Interest exceeds withdrawal - using COLLATERAL_BURN "
            "calculation (ceil rounding)",
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Perform calculation for logging/debugging (value not used since we skip validation)
        context.calculate(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            raw_amount=raw_amount,
            index=event.index,
            token_revision=token_revision,
        )

        # Set scaled_amount=None to skip validation since we overrode the calculation
        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=None,  # Skip validation
        )

    def _handle_standard_burn(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Handle standard WITHDRAW burn event.

        Returns:
            The computed value.

        """
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
