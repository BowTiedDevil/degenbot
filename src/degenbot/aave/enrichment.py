"""Main enrichment service for scaled token events."""

from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from sqlalchemy.orm import Session

from degenbot.aave.calculator import ScaledAmountCalculator
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.extraction import RawAmountExtractor
from degenbot.aave.models import (
    EnrichedCollateralBurnEvent,
    EnrichedCollateralInterestBurnEvent,
    EnrichedCollateralInterestMintEvent,
    EnrichedCollateralMintEvent,
    EnrichedCollateralTransferEvent,
    EnrichedDebtBurnEvent,
    EnrichedDebtInterestBurnEvent,
    EnrichedDebtInterestMintEvent,
    EnrichedDebtMintEvent,
    EnrichedDebtTransferEvent,
    EnrichedGhoDebtBurnEvent,
    EnrichedGhoDebtInterestBurnEvent,
    EnrichedGhoDebtInterestMintEvent,
    EnrichedGhoDebtMintEvent,
    EnrichedGhoDebtTransferEvent,
    EnrichedScaledTokenEvent,
    EnrichmentError,
)
from degenbot.database.models.aave import AaveV3Asset
from degenbot.database.models.erc20 import Erc20TokenTable

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class ScaledEventEnricher:
    """
    Enriches ScaledTokenEvent with calculated scaled amounts.

    This is the main entry point for event enrichment. It:
    1. Extracts raw amounts from Pool events
    2. Calculates scaled amounts using TokenMath
    3. Creates validated Pydantic event objects
    4. Raises immediately on any error
    """

    def __init__(
        self,
        pool_revision: int,
        token_revisions: dict[ChecksumAddress, int],
        session: Session,
    ) -> None:
        self.pool_revision = pool_revision
        self.token_revisions = token_revisions
        self.session = session

    def enrich(
        self,
        scaled_event: "ScaledTokenEvent",
        operation: "Operation",
    ) -> EnrichedScaledTokenEvent:
        """
        Enrich a single ScaledTokenEvent.

        Args:
            scaled_event: The raw ScaledTokenEvent to enrich
            operation: The Operation containing context

        Returns:
            EnrichedScaledTokenEvent with validated scaled amounts

        Raises:
            EnrichmentError: If extraction or calculation fails
        """
        # 1. Determine token revision
        token_address = ChecksumAddress(scaled_event.event["address"])
        token_revision = self._get_token_revision(token_address)

        # 2. Get underlying asset address
        underlying_asset = self._get_underlying_asset(token_address)

        # 3. Handle operations with or without pool events
        if operation.pool_event is None:
            # Check if this is an INTEREST_ACCRUAL operation
            if operation.operation_type.name == "INTEREST_ACCRUAL":
                # Interest accrual events don't have pool events.
                # The Mint event for interest accrual is emitted for tracking purposes only.
                # Interest accrual does NOT mint tokens or increase the scaled balance -
                # it only updates the user's stored index.
                # See Aave V3 aToken contract _transfer function (rev_1.sol:2844-2846)
                raw_amount = scaled_event.amount
                scaled_amount = 0  # Interest accrual does not change scaled balance
            elif operation.operation_type.name == "MINT_TO_TREASURY":
                # MINT_TO_TREASURY: The event amount includes underlying amount + interest accrued.
                # The actual underlying amount is amount - balance_increase.
                # See Aave V3 aToken contract _mintScaled and mintToTreasury.
                if scaled_event.index is None:
                    msg = f"MINT_TO_TREASURY event has no index: {scaled_event}"
                    raise EnrichmentError(msg)

                # Calculate actual raw amount (underlying being minted)
                balance_increase = scaled_event.balance_increase or 0
                raw_amount = scaled_event.amount - balance_increase

                # Calculate scaled amount using TokenMath
                calculator = ScaledAmountCalculator(
                    pool_revision=self.pool_revision,
                    token_revision=token_revision,
                )
                scaled_amount = calculator.calculate(
                    event_type=scaled_event.event_type.value,
                    raw_amount=raw_amount,
                    index=scaled_event.index,
                )
            else:
                # Internal transfers (BALANCE_TRANSFER) have no pool event
                # For transfers, raw_amount = scaled_amount (no index-based calculation)
                raw_amount = scaled_event.amount
                scaled_amount = scaled_event.amount
        # ERC20 transfers don't have an index - use amount directly
        elif scaled_event.event_type == ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER:
            raw_amount = scaled_event.amount
            scaled_amount = scaled_event.amount
        else:
            # Extract raw amount from pool event and calculate scaled amount
            extractor = RawAmountExtractor(
                pool_event=operation.pool_event,
                pool_revision=self.pool_revision,
            )
            raw_amount = extractor.extract()

            # Calculate scaled amount using TokenMath
            calculator = ScaledAmountCalculator(
                pool_revision=self.pool_revision,
                token_revision=token_revision,
            )

            if scaled_event.index is None:
                msg = f"Scaled event has no index: {scaled_event}"
                raise EnrichmentError(msg)

            scaled_amount = calculator.calculate(
                event_type=scaled_event.event_type.value,
                raw_amount=raw_amount,
                index=scaled_event.index,
            )

        # 5. Create appropriate enriched event type
        return self._create_enriched_event(
            scaled_event=scaled_event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
            token_revision=token_revision,
            token_address=token_address,
            underlying_asset=underlying_asset,
        )

    def _get_token_revision(self, token_address: ChecksumAddress) -> int:
        """Get token revision from database or cache."""
        revision = self.token_revisions.get(token_address)
        if revision is None:
            # Fetch from database
            revision = self._fetch_token_revision_from_db(token_address)
            self.token_revisions[token_address] = revision
        return revision

    def _fetch_token_revision_from_db(
        self,
        token_address: ChecksumAddress,
    ) -> int:
        """Fetch token revision from database."""

        # Query for a_token match
        a_token_asset = (
            self.session
            .query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if a_token_asset is not None:
            return a_token_asset.a_token_revision

        # Query for v_token match
        v_token_asset = (
            self.session
            .query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if v_token_asset is not None:
            return v_token_asset.v_token_revision

        msg = f"Could not find asset for token {token_address}"
        raise EnrichmentError(msg)

    def _get_underlying_asset(
        self,
        token_address: ChecksumAddress,
    ) -> ChecksumAddress:
        """Get underlying asset address for a token."""

        # Query by joining with a_token or v_token
        asset = (
            self.session
            .query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if asset is None:
            asset = (
                self.session
                .query(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
                .filter(Erc20TokenTable.address == token_address)
                .first()
            )

        if asset is None:
            msg = f"Could not find underlying asset for token {token_address}"
            raise EnrichmentError(msg)

        return ChecksumAddress(asset.underlying_token.address)

    def _create_enriched_event(
        self,
        *,
        scaled_event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int,
        token_revision: int,
        token_address: ChecksumAddress,
        underlying_asset: ChecksumAddress,
    ) -> EnrichedScaledTokenEvent:
        """Create the appropriate enriched event type."""
        event_type = scaled_event.event_type
        is_interest_accrual = operation.operation_type.name == "INTEREST_ACCRUAL"

        # Map event type to enriched class
        # For INTEREST_ACCRUAL, use interest-specific event types
        class_map: dict[ScaledTokenEventType, type[EnrichedScaledTokenEvent]]
        if is_interest_accrual:
            class_map = {
                ScaledTokenEventType.COLLATERAL_MINT: EnrichedCollateralInterestMintEvent,
                ScaledTokenEventType.COLLATERAL_BURN: EnrichedCollateralInterestBurnEvent,
                ScaledTokenEventType.DEBT_MINT: EnrichedDebtInterestMintEvent,
                ScaledTokenEventType.DEBT_BURN: EnrichedDebtInterestBurnEvent,
                ScaledTokenEventType.GHO_DEBT_MINT: EnrichedGhoDebtInterestMintEvent,
                ScaledTokenEventType.GHO_DEBT_BURN: EnrichedGhoDebtInterestBurnEvent,
            }
        else:
            class_map = {
                ScaledTokenEventType.COLLATERAL_MINT: EnrichedCollateralMintEvent,
                ScaledTokenEventType.COLLATERAL_BURN: EnrichedCollateralBurnEvent,
                ScaledTokenEventType.COLLATERAL_TRANSFER: EnrichedCollateralTransferEvent,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER: EnrichedCollateralTransferEvent,
                ScaledTokenEventType.DEBT_MINT: EnrichedDebtMintEvent,
                ScaledTokenEventType.DEBT_BURN: EnrichedDebtBurnEvent,
                ScaledTokenEventType.DEBT_TRANSFER: EnrichedDebtTransferEvent,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER: EnrichedDebtTransferEvent,
                ScaledTokenEventType.GHO_DEBT_MINT: EnrichedGhoDebtMintEvent,
                ScaledTokenEventType.GHO_DEBT_BURN: EnrichedGhoDebtBurnEvent,
                ScaledTokenEventType.GHO_DEBT_TRANSFER: EnrichedGhoDebtTransferEvent,
            }

        enriched_class = class_map.get(event_type)
        if enriched_class is None:
            msg = f"Unknown event type: {event_type.value}"
            raise EnrichmentError(msg)

        # Build base kwargs (common to all event types)
        # For interest accrual, get the correct event_type from the class
        if is_interest_accrual:
            # Map the event_type to interest-specific event type strings
            # (values are Literal strings used by Pydantic models)
            interest_event_type_map: dict[ScaledTokenEventType, str] = {
                ScaledTokenEventType.COLLATERAL_MINT: "collateral_interest_mint",
                ScaledTokenEventType.COLLATERAL_BURN: "collateral_interest_burn",
                ScaledTokenEventType.DEBT_MINT: "debt_interest_mint",
                ScaledTokenEventType.DEBT_BURN: "debt_interest_burn",
                ScaledTokenEventType.GHO_DEBT_MINT: "gho_debt_interest_mint",
                ScaledTokenEventType.GHO_DEBT_BURN: "gho_debt_interest_burn",
            }
            actual_event_type = interest_event_type_map.get(event_type, event_type.value)
        else:
            # Map ERC20 transfer types to their base types for Pydantic models
            event_type_map: dict[ScaledTokenEventType, str] = {
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER: "collateral_transfer",
                ScaledTokenEventType.ERC20_DEBT_TRANSFER: "debt_transfer",
            }
            actual_event_type = event_type_map.get(event_type, event_type.value)

        kwargs: dict[str, Any] = {
            "event": scaled_event.event,
            "event_type": actual_event_type,
            "user_address": scaled_event.user_address,
            "raw_amount": raw_amount,
            "scaled_amount": scaled_amount,
            "pool_revision": self.pool_revision,
            "token_revision": token_revision,
            "token_address": token_address,
            "underlying_asset": underlying_asset,
        }

        # Interest accrual events need index and balance_increase for processor validation
        if is_interest_accrual:
            if scaled_event.index is None:
                msg = f"Interest accrual event has no index: {scaled_event}"
                raise EnrichmentError(msg)
            kwargs["index"] = scaled_event.index
            kwargs["balance_increase"] = scaled_event.balance_increase or 0
        # Index-scaled events (mint/burn) require index and balance_increase
        elif event_type in {
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            if scaled_event.index is None:
                msg = f"Index-scaled event has no index: {scaled_event}"
                raise EnrichmentError(msg)
            kwargs["index"] = scaled_event.index
            kwargs["balance_increase"] = scaled_event.balance_increase

        # Add type-specific fields
        if event_type == ScaledTokenEventType.COLLATERAL_MINT and not is_interest_accrual:
            kwargs["caller_address"] = scaled_event.caller_address
        elif event_type == ScaledTokenEventType.COLLATERAL_BURN and not is_interest_accrual:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["target_address"] = scaled_event.target_address
        elif event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            # Transfer events need from_address and to_address
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["to_address"] = scaled_event.target_address or scaled_event.user_address
        elif event_type in {
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        }:
            # Transfer events need from_address and to_address
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["to_address"] = scaled_event.target_address or scaled_event.user_address
        elif event_type == ScaledTokenEventType.DEBT_MINT and not is_interest_accrual:
            kwargs["caller_address"] = scaled_event.caller_address
        elif event_type == ScaledTokenEventType.DEBT_BURN and not is_interest_accrual:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["target_address"] = scaled_event.target_address
        elif event_type == ScaledTokenEventType.GHO_DEBT_MINT:
            kwargs["caller_address"] = scaled_event.caller_address
            if is_interest_accrual:
                # Interest accrual - use actual discount from event if available
                kwargs["discount_percent"] = getattr(scaled_event, "discount_percent", 0)
                kwargs["discount_scaled"] = getattr(scaled_event, "discount_scaled", 0)
            else:
                # TODO: Add GHO discount fields from transaction context
                kwargs["discount_percent"] = 0  # Placeholder
                kwargs["discount_scaled"] = 0  # Placeholder
        elif event_type == ScaledTokenEventType.GHO_DEBT_BURN:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["target_address"] = scaled_event.target_address
            if is_interest_accrual:
                # Interest accrual - use actual discount from event if available
                kwargs["discount_percent"] = getattr(scaled_event, "discount_percent", 0)
                kwargs["discount_scaled"] = getattr(scaled_event, "discount_scaled", 0)
            else:
                # TODO: Add GHO discount fields from transaction context
                kwargs["discount_percent"] = 0  # Placeholder
                kwargs["discount_scaled"] = 0  # Placeholder
        elif event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["to_address"] = scaled_event.user_address  # Placeholder
            kwargs["discount_scaled"] = 0  # Placeholder

        return enriched_class(**kwargs)
