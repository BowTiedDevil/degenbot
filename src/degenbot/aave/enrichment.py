"""Main enrichment service for scaled token events."""

from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from sqlalchemy.orm import Session

from degenbot.aave.calculator import ScaledAmountCalculator
from degenbot.aave.events import AaveV3PoolEvent, ScaledTokenEventType
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
from degenbot.aave.operation_types import OperationType
from degenbot.database.models.aave import AaveV3Asset
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.logging import logger

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

    _PLACEHOLDER_INT = 0

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

        # Track calculation type to detect overrides for validation skipping
        calculation_event_type: ScaledTokenEventType | None = None

        # 3. Handle operations with or without pool events
        if operation.pool_event is None:
            # Check if this is an INTEREST_ACCRUAL operation
            if operation.operation_type == OperationType.INTEREST_ACCRUAL:
                # Interest accrual events don't have pool events.
                # The Mint event for interest accrual is emitted for tracking purposes only.
                # Interest accrual does NOT mint tokens or increase the scaled balance -
                # it only updates the user's stored index.
                # See Aave V3 aToken contract _transfer function (rev_1.sol:2844-2846)
                raw_amount = scaled_event.amount
                scaled_amount = 0  # Interest accrual does not change scaled balance
            elif operation.operation_type == OperationType.MINT_TO_TREASURY:
                # MINT_TO_TREASURY requires position data (current balance and last_index)
                # to correctly calculate accruedToTreasury. The enrichment layer doesn't
                # have access to position data, so leave scaled_amount as None.
                # The correct calculation is performed in aave.py with position context.
                # See debug/aave/0014 - MINT_TO_TREASURY AccruedToTreasury Calculation Error.md
                raw_amount = scaled_event.amount
                scaled_amount = None
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
            # Special handling for LIQUIDATION: use different extractors for debt vs collateral
            if operation.operation_type in {
                OperationType.LIQUIDATION,
                OperationType.GHO_LIQUIDATION,
            }:
                if operation.pool_event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                    # Determine which amount to extract based on event type
                    if scaled_event.event_type in {
                        ScaledTokenEventType.DEBT_BURN,
                        ScaledTokenEventType.GHO_DEBT_BURN,
                        ScaledTokenEventType.DEBT_TRANSFER,
                        ScaledTokenEventType.ERC20_DEBT_TRANSFER,
                    }:
                        # Debt events use debtToCover
                        raw_amount = RawAmountExtractor.extract_liquidation_debt(
                            operation.pool_event
                        )
                        # Pool Revision 9+ passes pre-scaled amounts to token contracts
                        # The Pool calculates scaledAmount = debtToCover.rayDivFloor(index)
                        # and passes it to vToken.burn(). We must calculate this ourselves.
                        # See debug/aave/0044 for details
                        if self.pool_revision >= 9:  # noqa: PLR2004
                            # Calculate scaled amount from debtToCover using the index
                            # from the burn event: scaledAmount = debtToCover / index
                            # (floor division)
                            assert scaled_event.index is not None
                            calculator = ScaledAmountCalculator(
                                pool_revision=self.pool_revision,
                                token_revision=token_revision,
                            )
                            scaled_amount = calculator.calculate(
                                event_type=ScaledTokenEventType.DEBT_BURN,
                                raw_amount=raw_amount,
                                index=scaled_event.index,
                            )
                            logger.debug(
                                f"ENRICHMENT: Pool Rev {self.pool_revision} LIQUIDATION "
                                f"calculated scaled amount: {scaled_amount} "
                                f"from debtToCover={raw_amount} / index={scaled_event.index}"
                            )
                            calculation_event_type = scaled_event.event_type
                            # Skip to event creation
                            return self._create_enriched_event(
                                scaled_event=scaled_event,
                                operation=operation,
                                raw_amount=raw_amount,
                                scaled_amount=scaled_amount,
                                token_revision=token_revision,
                                token_address=token_address,
                                underlying_asset=underlying_asset,
                            )
                    elif scaled_event.event_type in {
                        ScaledTokenEventType.COLLATERAL_BURN,
                        ScaledTokenEventType.COLLATERAL_TRANSFER,
                        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
                    }:
                        # Collateral events use liquidatedCollateralAmount
                        raw_amount = RawAmountExtractor.extract_liquidation_collateral(
                            operation.pool_event
                        )
                    else:
                        # Default to debt amount for unknown event types
                        raw_amount = RawAmountExtractor.extract_liquidation_debt(
                            operation.pool_event
                        )
                else:
                    extractor = RawAmountExtractor(
                        pool_event=operation.pool_event,
                        pool_revision=self.pool_revision,
                    )
                    raw_amount = extractor.extract()
            elif (
                # Special case: When interest exceeds withdrawal, the Mint event's amount
                # represents the net interest (interest - withdrawal), not the actual withdrawal.
                # But we need the withdrawal amount to calculate the scaled burn.
                # Detection: In a WITHDRAW operation, if COLLATERAL_MINT has
                # amount < balance_increase, it means interest > withdrawal. Use the pool event's
                # withdraw amount.
                operation.operation_type == OperationType.WITHDRAW
                and scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT
                and scaled_event.balance_increase is not None
                and scaled_event.amount < scaled_event.balance_increase
            ):
                # Interest exceeds withdrawal - extract the actual withdrawal amount
                extractor = RawAmountExtractor(
                    pool_event=operation.pool_event,
                    pool_revision=self.pool_revision,
                )
                raw_amount = extractor.extract()
                logger.debug(
                    f"ENRICHMENT: Interest exceeds withdrawal - using withdraw amount "
                    f"{raw_amount} for burn calculation"
                )
            elif (
                # Special case: When interest exceeds repayment, the VariableDebtToken emits
                # a Mint event with amount = balance_increase - repay_amount (net debt increase).
                # But we need the actual repay amount to calculate the scaled burn.
                # Detection: In a REPAY operation, if DEBT_MINT is emitted, interest > repayment.
                operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
                and scaled_event.event_type == ScaledTokenEventType.DEBT_MINT
                and scaled_event.balance_increase is not None
            ):
                # Interest exceeds repayment - extract the actual repay amount
                extractor = RawAmountExtractor(
                    pool_event=operation.pool_event,
                    pool_revision=self.pool_revision,
                )
                raw_amount = extractor.extract()
                logger.debug(
                    f"ENRICHMENT: Interest exceeds repayment - using repay amount "
                    f"{raw_amount} for burn calculation"
                )
            else:
                # Non-liquidation operations use standard extraction
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

            # Special case: When interest exceeds withdrawal amount, the aToken contract
            # emits a Mint event instead of a Burn event (AToken rev_4.sol:2836-2839).
            # In this case, use COLLATERAL_BURN calculation (ceil rounding) instead of
            # COLLATERAL_MINT (floor rounding) to match Pool rev 9+ contract behavior.
            if (
                operation.operation_type == OperationType.WITHDRAW
                and scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT
                and scaled_event.balance_increase is not None
                and scaled_event.amount < scaled_event.balance_increase
            ):
                # Use COLLATERAL_BURN for burn rounding (ceil)
                calculation_event_type = ScaledTokenEventType.COLLATERAL_BURN
                logger.debug(
                    "ENRICHMENT: Interest exceeds withdrawal - using COLLATERAL_BURN "
                    "calculation (ceil rounding)"
                )
            elif (
                # Special case: When interest exceeds repayment amount in
                # REPAY_WITH_ATOKENS, the aToken contract emits a Mint event with
                # amount = balance_increase - repay_amount. Use COLLATERAL_BURN
                # calculation (ceil rounding) to match contract behavior.
                operation.operation_type == OperationType.REPAY_WITH_ATOKENS
                and scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT
                and scaled_event.balance_increase is not None
                and scaled_event.amount < scaled_event.balance_increase
            ):
                # Use COLLATERAL_BURN for burn rounding (ceil)
                calculation_event_type = ScaledTokenEventType.COLLATERAL_BURN
                logger.debug(
                    "ENRICHMENT: Interest exceeds repayment - using COLLATERAL_BURN "
                    "calculation (ceil rounding)"
                )
            elif (
                # Special case: When interest exceeds repayment amount, the VariableDebtToken
                # emits a Mint event instead of a Burn event (VariableDebtToken _burnScaled).
                # In this case, use DEBT_BURN calculation (floor rounding) instead of
                # DEBT_MINT (ceil rounding) to match contract behavior.
                # Also handles GHO_DEBT_MINT for GHO tokens.
                # Includes REPAY_WITH_ATOKENS which uses the same debt repayment logic.
                operation.operation_type
                in {
                    OperationType.GHO_REPAY,
                    OperationType.REPAY,
                    OperationType.REPAY_WITH_ATOKENS,
                }
                and scaled_event.event_type
                in {
                    ScaledTokenEventType.DEBT_MINT,
                    ScaledTokenEventType.GHO_DEBT_MINT,
                }
                and scaled_event.balance_increase is not None
            ):
                # Use DEBT_BURN for burn rounding (floor)
                calculation_event_type = ScaledTokenEventType.DEBT_BURN
                logger.debug(
                    "ENRICHMENT: Interest exceeds repayment - using DEBT_BURN "
                    "calculation (floor rounding)"
                )
            elif (
                # Special case: In LIQUIDATION operations, when the debt repayment is less
                # than the accrued interest, the VariableDebtToken emits a Mint event
                # representing a net debt increase (balance_increase - amount).
                # This should be treated as a debt burn (net increase) for correct balance
                # calculation.
                operation.operation_type
                in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
                and scaled_event.event_type == ScaledTokenEventType.DEBT_MINT
                and scaled_event.balance_increase is not None
                and scaled_event.balance_increase > scaled_event.amount
            ):
                # Interest exceeds repayment: net debt increase
                # Use DEBT_BURN calculation (floor rounding) to correctly handle the net increase
                calculation_event_type = ScaledTokenEventType.DEBT_BURN
                raw_amount = scaled_event.balance_increase - scaled_event.amount
                logger.debug(
                    f"ENRICHMENT: LIQUIDATION net debt increase - using DEBT_BURN "
                    f"calculation with raw_amount={raw_amount}"
                )
            else:
                calculation_event_type = scaled_event.event_type

            # Calculate scaled amount using the appropriate method
            scaled_amount = calculator.calculate(
                event_type=calculation_event_type,
                raw_amount=raw_amount,
                index=scaled_event.index,
            )

            # Special case: When enrichment overrides the calculation type
            # (e.g., REPAY + DEBT_MINT with interest > repayment), skip validation
            # by setting scaled_amount=None. The processing layer recalculates
            # the amount anyway for these cases.
            # See debug/aave/0031 for details.
            #
            # NOTE: Do NOT set scaled_amount=None for REPAY/GHO_REPAY with DEBT_MINT/GHO_DEBT_MINT.
            # The processing layer now uses the enriched scaled_amount directly to avoid
            # 1 wei rounding errors from deriving the amount from Mint event fields.
            # See debug/aave/0037 - GHO REPAY Uses Mint Event Instead of Repay Event Amount.md
            if calculation_event_type != scaled_event.event_type and not (
                operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
                and scaled_event.event_type
                in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
            ):
                logger.debug(
                    f"ENRICHMENT: Overriding {scaled_event.event_type.name} with "
                    f"{calculation_event_type.name} - skipping validation by setting "
                    f"scaled_amount=None"
                )
                scaled_amount = None

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
        scaled_amount: int | None,
        token_revision: int,
        token_address: ChecksumAddress,
        underlying_asset: ChecksumAddress,
    ) -> EnrichedScaledTokenEvent:
        """Create the appropriate enriched event type."""
        event_type = scaled_event.event_type
        is_interest_accrual = operation.operation_type == OperationType.INTEREST_ACCRUAL

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
            msg = f"Unknown event type: {event_type}"
            raise EnrichmentError(msg)

        # Build base kwargs (common to all event types)
        # For interest accrual, map to the interest-specific event type
        if is_interest_accrual:
            interest_event_type_map: dict[ScaledTokenEventType, ScaledTokenEventType] = {
                ScaledTokenEventType.COLLATERAL_MINT: ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
                ScaledTokenEventType.COLLATERAL_BURN: ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
                ScaledTokenEventType.DEBT_MINT: ScaledTokenEventType.DEBT_INTEREST_MINT,
                ScaledTokenEventType.DEBT_BURN: ScaledTokenEventType.DEBT_INTEREST_BURN,
                ScaledTokenEventType.GHO_DEBT_MINT: ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
                ScaledTokenEventType.GHO_DEBT_BURN: ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
            }
            actual_event_type = interest_event_type_map.get(event_type, event_type)
        else:
            # Map ERC20 transfer types to their base types
            event_type_map: dict[ScaledTokenEventType, ScaledTokenEventType] = {
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER: (
                    ScaledTokenEventType.COLLATERAL_TRANSFER
                ),
                ScaledTokenEventType.ERC20_DEBT_TRANSFER: ScaledTokenEventType.DEBT_TRANSFER,
            }
            actual_event_type = event_type_map.get(event_type, event_type)

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
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
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
                kwargs["discount_percent"] = self._PLACEHOLDER_INT
                kwargs["discount_scaled"] = self._PLACEHOLDER_INT
        elif event_type == ScaledTokenEventType.GHO_DEBT_BURN:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["target_address"] = scaled_event.target_address
            if is_interest_accrual:
                # Interest accrual - use actual discount from event if available
                kwargs["discount_percent"] = getattr(scaled_event, "discount_percent", 0)
                kwargs["discount_scaled"] = getattr(scaled_event, "discount_scaled", 0)
            else:
                kwargs["discount_percent"] = self._PLACEHOLDER_INT
                kwargs["discount_scaled"] = self._PLACEHOLDER_INT
        elif event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["from_address"] = scaled_event.from_address or scaled_event.user_address
            kwargs["to_address"] = scaled_event.user_address
            kwargs["discount_scaled"] = self._PLACEHOLDER_INT

        return enriched_class(**kwargs)
