"""LIQUIDATION/GHO_LIQUIDATION operation handler.

LIQUIDATION operations emit both DEBT_BURN and COLLATERAL_BURN events.
GHO_LIQUIDATION emits GHO_DEBT_BURN and COLLATERAL_BURN events.

The Pool LiquidationCall event contains:
- debtToCover: The debt amount being repaid
- liquidatedCollateralAmount: The collateral being seized

Special cases:
1. Debt vs collateral extraction: Use different extractors based on event type
2. Pool Revision 9+: Pre-scaled amounts passed to token contracts
3. Net debt increase: When balance_increase > amount on DEBT_MINT

See debug/aave/0044 for Pool rev 9+ details.
"""

from typing import TYPE_CHECKING, ClassVar

from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent, ScaledTokenEventType
from degenbot.aave.extraction import RawAmountExtractor
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType
from degenbot.logging import logger

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


# Pool revision where pre-scaled amounts were introduced
POOL_REV_PRE_SCALED = 9


class LiquidationHandler:
    """Handle enrichment for LIQUIDATION and GHO_LIQUIDATION operations."""

    operation_types: ClassVar[set[OperationType]] = {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
    }

    # Event types for debt extraction
    DEBT_EVENT_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.DEBT_BURN,
        ScaledTokenEventType.GHO_DEBT_BURN,
        ScaledTokenEventType.DEBT_TRANSFER,
        ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        ScaledTokenEventType.DEBT_MINT,
    }

    # Event types for collateral extraction
    COLLATERAL_EVENT_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.COLLATERAL_BURN,
        ScaledTokenEventType.COLLATERAL_TRANSFER,
        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
    }

    # ERC20 transfer events have no Aave index — bypass index-based scaling
    ERC20_TRANSFER_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        ScaledTokenEventType.ERC20_DEBT_TRANSFER,
    }

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Enrich a LIQUIDATION or GHO_LIQUIDATION event.

        Handles debt burns, collateral burns, and special cases.
        """
        if operation.pool_event is None:
            msg = "LIQUIDATION operation has no pool event"
            raise EnrichmentError(msg)

        event_type = event.event_type

        # ERC20 transfers within liquidation bypass index-based scaling
        # Standard ERC20 Transfer events don't carry an Aave index.
        # raw_amount = scaled_amount (amount is already in scaled units).
        # Legacy code handled this before the liquidation branch.
        if event_type in self.ERC20_TRANSFER_TYPES:
            return self._handle_erc20_transfer(event, operation, context)

        # Check for net debt increase case (DEBT_MINT with balance_increase > amount)
        if (
            event_type == ScaledTokenEventType.DEBT_MINT
            and event.balance_increase is not None
            and event.balance_increase > event.amount
        ):
            return self._handle_net_debt_increase(event, operation, context)

        # Handle Pool rev 9+ debt burns (pre-scaled amounts)
        if (
            context.pool_revision >= POOL_REV_PRE_SCALED
            and event_type in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
            and self._is_liquidation_call_event(operation.pool_event)
        ):
            return self._handle_pool_rev9_debt_burn(event, operation, context)

        # Standard handling for debt and collateral events
        if event.index is None:
            msg = "LIQUIDATION event has no index"
            raise EnrichmentError(msg)

        # Extract raw amount based on event type
        raw_amount = self._extract_liquidation_amount(
            pool_event=operation.pool_event,
            event_type=event_type,
        )

        # Get token revision for calculation
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Determine calculation event type
        calc_event_type = self._get_calculation_event_type(event_type)

        # Calculate scaled amount
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

    def _handle_erc20_transfer(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Handle ERC20 transfer events within a liquidation operation.

        Standard ERC20 Transfer events don't carry an Aave index.
        The amount is already in scaled units, so raw_amount = scaled_amount.
        """
        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=event.amount,
            scaled_amount=event.amount,
        )

    def _is_liquidation_call_event(self, pool_event: LogReceipt) -> bool:  # noqa: PLR6301
        """Check if the pool event is a LiquidationCall."""
        return pool_event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value

    def _extract_liquidation_amount(
        self,
        pool_event: LogReceipt,
        event_type: ScaledTokenEventType,
    ) -> int:
        """Extract the appropriate amount from a LiquidationCall event."""
        if event_type in self.DEBT_EVENT_TYPES:
            return RawAmountExtractor.extract_liquidation_debt(pool_event)
        if event_type in self.COLLATERAL_EVENT_TYPES:
            return RawAmountExtractor.extract_liquidation_collateral(pool_event)

        # Default to debt amount for unknown types
        return RawAmountExtractor.extract_liquidation_debt(pool_event)

    def _get_calculation_event_type(  # noqa: PLR6301
        self, event_type: ScaledTokenEventType
    ) -> ScaledTokenEventType:
        """Get the event type to use for calculation."""
        # Most events use their own type for calculation
        return event_type

    def _handle_pool_rev9_debt_burn(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Handle Pool Revision 9+ debt burns with pre-scaled amounts.

        Pool rev 9+ calculates scaledAmount = debtToCover.rayDivFloor(index)
        and passes it to vToken.burn(). We must calculate this ourselves.
        See debug/aave/0044 for details.
        """
        assert operation.pool_event is not None
        if event.index is None:
            msg = "LIQUIDATION event has no index"
            raise EnrichmentError(msg)

        # Extract debtToCover
        raw_amount = RawAmountExtractor.extract_liquidation_debt(operation.pool_event)

        # Get token revision
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Calculate scaled amount using DEBT_BURN (floor division)
        scaled_amount = context.calculate(
            event_type=ScaledTokenEventType.DEBT_BURN,
            raw_amount=raw_amount,
            index=event.index,
            token_revision=token_revision,
        )

        logger.debug(
            f"ENRICHMENT: Pool Rev {context.pool_revision} LIQUIDATION "
            f"calculated scaled amount: {scaled_amount} "
            f"from debtToCover={raw_amount} / index={event.index}"
        )

        return context.build_enriched_event(
            event=event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
        )

    def _handle_net_debt_increase(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent":
        """
        Handle the net debt increase case during liquidation.

        When balance_increase > amount on DEBT_MINT, it means the debt repayment
        was less than the accrued interest. This is a net debt increase.
        Use DEBT_BURN calculation with raw_amount = balance_increase - amount.
        """
        if event.index is None:
            msg = "LIQUIDATION event has no index"
            raise EnrichmentError(msg)

        # Calculate net increase
        raw_amount = event.balance_increase - event.amount

        logger.debug(
            f"ENRICHMENT: LIQUIDATION net debt increase - using DEBT_BURN "
            f"calculation with raw_amount={raw_amount}"
        )

        # Get token revision
        token_address = ChecksumAddress(event.event["address"])
        token_revision = context.get_token_revision(token_address)

        # Use DEBT_BURN calculation (floor rounding)
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
