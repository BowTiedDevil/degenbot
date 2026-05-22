"""INTEREST_ACCRUAL operation handler.

Interest accrual happens when index changes affect existing balances.
Formula: interest = scaled_balance * (new_index - old_index) / RAY

Interest accrual events don't have pool events. The Mint event for interest
accrual is emitted for tracking purposes only. Interest accrual does NOT mint
tokens or increase the scaled balance - it only updates the user's stored index.

See Aave V3 aToken contract _transfer function (rev_1.sol:2844-2846)
"""

from typing import TYPE_CHECKING, ClassVar

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class InterestAccrualHandler:
    """Handle enrichment for INTEREST_ACCRUAL operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.INTEREST_ACCRUAL}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """Enrich an interest accrual event.

        Interest accrual does not change scaled balance, so scaled_amount = 0.
        The raw_amount is the interest amount in underlying units.

        Returns:
            The computed value.

        """
        # Interest accrual events don't have pool events
        # The Mint/Burn event amount is the interest accrued
        raw_amount = event.amount
        scaled_amount = 0  # Interest accrual does not change scaled balance

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )
