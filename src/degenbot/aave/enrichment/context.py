"""Shared context for enrichment handlers."""

from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from sqlalchemy.orm import Session
from web3.types import LogReceipt

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

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class EnrichmentContext:
    """
    Shared context providing services for enrichment handlers.

    Encapsulates:
    - Token revision lookup (with caching)
    - Underlying asset resolution
    - Raw amount extraction from Pool events
    - Scaled amount calculation
    - Enriched event construction
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
        self._calculator_cache: dict[tuple[int, int], ScaledAmountCalculator] = {}

    def get_token_revision(self, token_address: ChecksumAddress) -> int:
        """Get token revision from cache or database."""
        revision = self.token_revisions.get(token_address)
        if revision is None:
            revision = self._fetch_token_revision_from_db(token_address)
            self.token_revisions[token_address] = revision
        return revision

    def _fetch_token_revision_from_db(self, token_address: ChecksumAddress) -> int:
        """Fetch token revision from database."""
        # Query for a_token match
        a_token_asset = (
            self.session.query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if a_token_asset is not None:
            return a_token_asset.a_token_revision

        # Query for v_token match
        v_token_asset = (
            self.session.query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if v_token_asset is not None:
            return v_token_asset.v_token_revision

        msg = f"Could not find asset for token {token_address}"
        raise EnrichmentError(msg)

    def get_underlying_asset(self, token_address: ChecksumAddress) -> ChecksumAddress:
        """Get underlying asset address for a token."""
        asset = (
            self.session.query(AaveV3Asset)
            .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
            .filter(Erc20TokenTable.address == token_address)
            .first()
        )

        if asset is None:
            asset = (
                self.session.query(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
                .filter(Erc20TokenTable.address == token_address)
                .first()
            )

        if asset is None:
            msg = f"Could not find underlying asset for token {token_address}"
            raise EnrichmentError(msg)

        return ChecksumAddress(asset.underlying_token.address)

    def get_calculator(
        self,
        pool_revision: int,
        token_revision: int,
    ) -> ScaledAmountCalculator:
        """Get or create a calculator for the given revisions."""
        key = (pool_revision, token_revision)
        if key not in self._calculator_cache:
            self._calculator_cache[key] = ScaledAmountCalculator(
                pool_revision=pool_revision,
                token_revision=token_revision,
            )
        return self._calculator_cache[key]

    def extract_pool_amount(
        self,
        pool_event: LogReceipt,
        event_type: ScaledTokenEventType | None = None,
        operation_type: OperationType | None = None,
    ) -> int:
        """
        Extract raw amount from a Pool event.

        Special handling for liquidations: use different extractors for debt vs collateral.
        """
        if (
            operation_type in {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}
            and pool_event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value
        ):
            if event_type in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
                ScaledTokenEventType.DEBT_TRANSFER,
                ScaledTokenEventType.ERC20_DEBT_TRANSFER,
            }:
                return RawAmountExtractor.extract_liquidation_debt(pool_event)
            if event_type in {
                ScaledTokenEventType.COLLATERAL_BURN,
                ScaledTokenEventType.COLLATERAL_TRANSFER,
                ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            }:
                return RawAmountExtractor.extract_liquidation_collateral(pool_event)

        # Standard extraction
        extractor = RawAmountExtractor(
            pool_event=pool_event,
            pool_revision=self.pool_revision,
        )
        return extractor.extract()

    def calculate(
        self,
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        """Calculate scaled amount using TokenMath."""
        calculator = self.get_calculator(self.pool_revision, token_revision)
        return calculator.calculate(
            event_type=event_type,
            raw_amount=raw_amount,
            index=index,
        )

    def build_enriched_event(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
        """Create the appropriate enriched event type."""
        token_address = ChecksumAddress(event.event["address"])
        token_revision = self.get_token_revision(token_address)
        underlying_asset = self.get_underlying_asset(token_address)
        event_type = event.event_type
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
            "event": event.event,
            "event_type": actual_event_type,
            "user_address": event.user_address,
            "raw_amount": raw_amount,
            "scaled_amount": scaled_amount,
            "pool_revision": self.pool_revision,
            "token_revision": token_revision,
            "token_address": token_address,
            "underlying_asset": underlying_asset,
        }

        # Interest accrual events need index and balance_increase for processor validation
        if is_interest_accrual:
            if event.index is None:
                msg = f"Interest accrual event has no index: {event}"
                raise EnrichmentError(msg)
            kwargs["index"] = event.index
            kwargs["balance_increase"] = event.balance_increase or 0
        # Index-scaled events (mint/burn) require index and balance_increase
        elif event_type in {
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            if event.index is None:
                msg = f"Index-scaled event has no index: {event}"
                raise EnrichmentError(msg)
            kwargs["index"] = event.index
            kwargs["balance_increase"] = event.balance_increase

        # Add type-specific fields
        if event_type == ScaledTokenEventType.COLLATERAL_MINT and not is_interest_accrual:
            kwargs["caller_address"] = event.caller_address
        elif event_type == ScaledTokenEventType.COLLATERAL_BURN and not is_interest_accrual:
            kwargs["from_address"] = event.from_address or event.user_address
            kwargs["target_address"] = event.target_address
        elif event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        }:
            kwargs["from_address"] = event.from_address or event.user_address
            kwargs["to_address"] = event.target_address or event.user_address
        elif event_type == ScaledTokenEventType.DEBT_MINT and not is_interest_accrual:
            kwargs["caller_address"] = event.caller_address
        elif event_type == ScaledTokenEventType.DEBT_BURN and not is_interest_accrual:
            kwargs["from_address"] = event.from_address or event.user_address
            kwargs["target_address"] = event.target_address
        elif event_type == ScaledTokenEventType.GHO_DEBT_MINT:
            kwargs["caller_address"] = event.caller_address
            if is_interest_accrual:
                # Interest accrual - use actual discount from event if available
                kwargs["discount_percent"] = getattr(event, "discount_percent", 0)
                kwargs["discount_scaled"] = getattr(event, "discount_scaled", 0)
            else:
                kwargs["discount_percent"] = self._PLACEHOLDER_INT
                kwargs["discount_scaled"] = self._PLACEHOLDER_INT
        elif event_type == ScaledTokenEventType.GHO_DEBT_BURN:
            kwargs["from_address"] = event.from_address or event.user_address
            kwargs["target_address"] = event.target_address
            if is_interest_accrual:
                kwargs["discount_percent"] = getattr(event, "discount_percent", 0)
                kwargs["discount_scaled"] = getattr(event, "discount_scaled", 0)
            else:
                kwargs["discount_percent"] = self._PLACEHOLDER_INT
                kwargs["discount_scaled"] = self._PLACEHOLDER_INT
        elif event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["from_address"] = event.from_address or event.user_address
            kwargs["to_address"] = event.user_address
            kwargs["discount_scaled"] = self._PLACEHOLDER_INT

        return enriched_class(**kwargs)
