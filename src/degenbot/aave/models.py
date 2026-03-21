"""Pydantic models for enriched Aave token events."""

from typing import Annotated, ClassVar

from eth_typing import ChecksumAddress
from pydantic import (
    BaseModel,
    Field,
    PlainValidator,
    field_validator,
    model_validator,
)
from web3.types import LogReceipt

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.libraries.token_math import TokenMathFactory

# Custom type that accepts LogReceipt (containing HexBytes) without validation
LogReceiptField = Annotated[LogReceipt, PlainValidator(lambda x: x)]


class EnrichmentError(Exception):
    """
    Raised when scaled amount enrichment fails.
    """


class ScaledAmountValidationError(Exception):
    """Raised when scaled amount validation fails."""

    def __init__(
        self,
        *,
        message: str,
        event_type: str,
        pool_revision: int,
        token_revision: int,
        raw_amount: int,
        expected_scaled: int,
        actual_scaled: int,
        index: int,
        token_math_method: str,
    ) -> None:
        self.event_type = event_type
        self.pool_revision = pool_revision
        self.token_revision = token_revision
        self.raw_amount = raw_amount
        self.expected_scaled = expected_scaled
        self.actual_scaled = actual_scaled
        self.index = index
        self.token_math_method = token_math_method

        detail = (
            f"{message}\n"
            f"  Event Type: {event_type}\n"
            f"  Pool Revision: {pool_revision}\n"
            f"  Token Revision: {token_revision}\n"
            f"  Raw Amount: {raw_amount}\n"
            f"  Expected Scaled: {expected_scaled}\n"
            f"  Actual Scaled: {actual_scaled}\n"
            f"  Index: {index}\n"
            f"  TokenMath Method: {token_math_method}\n"
            f"  Difference: {abs(expected_scaled - actual_scaled)} wei"
        )
        super().__init__(detail)


def _get_token_math_method(event_type: ScaledTokenEventType) -> str:
    """Map event type to TokenMath method name."""
    mapping = {
        ScaledTokenEventType.COLLATERAL_MINT: "get_collateral_mint_scaled_amount",
        ScaledTokenEventType.COLLATERAL_BURN: "get_collateral_burn_scaled_amount",
        ScaledTokenEventType.COLLATERAL_TRANSFER: "get_collateral_transfer_scaled_amount",
        ScaledTokenEventType.DEBT_MINT: "get_debt_mint_scaled_amount",
        ScaledTokenEventType.DEBT_BURN: "get_debt_burn_scaled_amount",
        ScaledTokenEventType.GHO_DEBT_MINT: "get_debt_mint_scaled_amount",
        ScaledTokenEventType.GHO_DEBT_BURN: "get_debt_burn_scaled_amount",
    }
    if event_type not in mapping:
        msg = f"Unknown event type: {event_type}"
        raise EnrichmentError(msg)
    return mapping[event_type]


class BaseEnrichedScaledTokenEvent(BaseModel):
    """
    Base class for enriched scaled token events.

    Contains common fields. Subclasses must define specific validation for their event types.
    """

    model_config = {"frozen": True}

    # Core identity
    event: LogReceiptField
    event_type: ScaledTokenEventType
    user_address: ChecksumAddress

    # Operation context
    pool_revision: int = Field(description="Pool contract revision at time of operation")
    token_revision: int = Field(description="Token contract revision")

    # Amounts - both always present post-enrichment
    raw_amount: int = Field(description="Amount from Pool event (user input before scaling)")
    scaled_amount: int | None = Field(
        description="Scaled amount calculated by Pool using TokenMath"
    )

    # Token contract addresses for debugging
    token_address: ChecksumAddress = Field(description="Address of the scaled token contract")
    underlying_asset: ChecksumAddress = Field(description="Address of the underlying asset")

    COLLATERAL_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.COLLATERAL_MINT,
        ScaledTokenEventType.COLLATERAL_BURN,
        ScaledTokenEventType.COLLATERAL_TRANSFER,
        ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
        ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
    }

    DEBT_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.DEBT_MINT,
        ScaledTokenEventType.DEBT_BURN,
        ScaledTokenEventType.DEBT_TRANSFER,
        ScaledTokenEventType.DEBT_INTEREST_MINT,
        ScaledTokenEventType.DEBT_INTEREST_BURN,
        ScaledTokenEventType.GHO_DEBT_MINT,
        ScaledTokenEventType.GHO_DEBT_BURN,
        ScaledTokenEventType.GHO_DEBT_TRANSFER,
        ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
        ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
    }

    BURN_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.COLLATERAL_BURN,
        ScaledTokenEventType.DEBT_BURN,
        ScaledTokenEventType.GHO_DEBT_BURN,
        ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
        ScaledTokenEventType.DEBT_INTEREST_BURN,
        ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
    }

    MINT_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.COLLATERAL_MINT,
        ScaledTokenEventType.DEBT_MINT,
        ScaledTokenEventType.GHO_DEBT_MINT,
        ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
        ScaledTokenEventType.DEBT_INTEREST_MINT,
        ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
    }

    TRANSFER_TYPES: ClassVar[set[ScaledTokenEventType]] = {
        ScaledTokenEventType.COLLATERAL_TRANSFER,
        ScaledTokenEventType.DEBT_TRANSFER,
        ScaledTokenEventType.GHO_DEBT_TRANSFER,
    }

    @property
    def is_collateral(self) -> bool:
        return self.event_type in self.COLLATERAL_TYPES

    @property
    def is_debt(self) -> bool:
        return self.event_type in self.DEBT_TYPES

    @property
    def is_burn(self) -> bool:
        return self.event_type in self.BURN_TYPES

    @property
    def is_mint(self) -> bool:
        return self.event_type in self.MINT_TYPES

    @property
    def is_transfer(self) -> bool:
        return self.event_type in self.TRANSFER_TYPES


class IndexScaledEvent(BaseEnrichedScaledTokenEvent):
    """
    Base class for index-scaled events (mint/burn).

    These events require validation that recalculates scaled_amount
    using the liquidity/borrow index.
    """

    # Interest accrual context - required for index-based validation
    index: int = Field(description="Current liquidity/borrow index used for scaling")
    balance_increase: int | None = Field(
        default=None, description="Interest accrued since last interaction"
    )

    @model_validator(mode="after")
    def validate_scaled_amount(self) -> "IndexScaledEvent":
        """Strict validation: recalculate scaled_amount and verify exact match."""

        pool_rev = self.pool_revision
        token_rev = self.token_revision
        raw = self.raw_amount
        idx = self.index
        event_type = self.event_type
        scaled = self.scaled_amount

        # Special case: Enrichment layer overrides calculation type
        # When enrichment switches calculation type (e.g., DEBT_MINT -> DEBT_BURN
        # for REPAY with interest > repayment), the calculated amount won't match
        # the event type's standard calculation. Skip validation in these cases
        # since the processing layer recalculates the amount anyway.
        # See debug/aave/0031 for details.
        if scaled is None:
            return self

        # Special case: Pool Revision 9+ LIQUIDATION debt amounts
        # For Pool Rev 9+, the debtToCover in LiquidationCall is already scaled
        # Skip validation since raw_amount == scaled_amount for these cases
        if pool_rev >= 9 and event_type == ScaledTokenEventType.DEBT_BURN:  # noqa: PLR2004
            return self

        # Calculate expected scaled amount
        token_math = TokenMathFactory.get_token_math_for_token_revision(token_rev)
        method_name = _get_token_math_method(event_type)
        method = getattr(token_math, method_name)

        expected = method(raw, idx)

        # Special case: Withdraw with interest exceeding withdrawal amount
        # When a COLLATERAL_MINT event is part of a WITHDRAW operation and
        # interest > withdrawal, the Pool uses burn rounding (ceil) but emits
        # a Mint event. Allow the ceil-calculated value.
        # Detection: raw < balance_increase means interest > withdrawal
        if (
            event_type == ScaledTokenEventType.COLLATERAL_MINT
            and self.balance_increase is not None
            and raw < self.balance_increase
        ):
            # Calculate expected with burn rounding (ceil)
            expected_burn = token_math.get_collateral_burn_scaled_amount(
                amount=raw, liquidity_index=idx
            )

            # Accept either mint (floor) or burn (ceil) rounding
            if scaled == expected_burn:
                return self  # Validation passes with burn rounding

        # Special case: REPAY with interest exceeding repayment amount
        # When a DEBT_MINT or GHO_DEBT_MINT event has balance_increase > 0,
        # it indicates interest accrued. For REPAY operations, the Pool uses
        # burn rounding (floor) but emits a Mint event. Allow the floor-calculated value.
        # See debug/aave/0037 for details.
        if (
            event_type
            in {
                ScaledTokenEventType.DEBT_MINT,
                ScaledTokenEventType.GHO_DEBT_MINT,
            }
            and self.balance_increase is not None
            and self.balance_increase > 0
        ):
            # Calculate expected with burn rounding (floor)
            expected_burn = token_math.get_debt_burn_scaled_amount(amount=raw, borrow_index=idx)

            # Accept the burn (floor) calculated value
            if scaled == expected_burn:
                return self  # Validation passes with burn rounding

        if scaled != expected:
            raise ScaledAmountValidationError(
                message="Scaled amount validation failed",
                event_type=str(event_type),
                pool_revision=pool_rev,
                token_revision=token_rev,
                raw_amount=raw,
                expected_scaled=expected,
                actual_scaled=scaled,
                index=idx,
                token_math_method=method_name,
            )

        return self


class TransferEvent(BaseEnrichedScaledTokenEvent):
    """
    Base class for transfer events.

    Transfers don't use index-based scaling, so raw_amount == scaled_amount.
    No validation needed since the Pool doesn't recalculate these amounts.
    """

    # Transfers have no index - amounts are directly transferred
    index: None = Field(default=None, exclude=True)
    balance_increase: None = Field(default=None, exclude=True)

    # Transfer-specific fields
    from_address: ChecksumAddress
    to_address: ChecksumAddress


class InterestAccrualEvent(BaseEnrichedScaledTokenEvent):
    """
    Base class for interest accrual events.

    Interest accrual happens when index changes affect existing balances.
    Formula: interest = scaled_balance * (new_index - old_index) / RAY

    These events don't follow TokenMath patterns since there's no Pool calculation.
    Validation is done by processors using stored previous_index.
    """

    # Interest accrual context
    index: int = Field(description="New index after interest accrual")
    balance_increase: int = Field(description="Interest amount in underlying units")


# Standard non-GHO events


class EnrichedCollateralMintEvent(IndexScaledEvent):
    """Enriched aToken mint event (supply)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.COLLATERAL_MINT
    caller_address: ChecksumAddress | None = Field(
        default=None, description="Address that initiated the mint (may differ from user)"
    )


class EnrichedCollateralBurnEvent(IndexScaledEvent):
    """Enriched aToken burn event (withdraw)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.COLLATERAL_BURN
    from_address: ChecksumAddress
    target_address: ChecksumAddress | None = Field(
        default=None, description="Address receiving underlying asset"
    )


class EnrichedCollateralTransferEvent(TransferEvent):
    """Enriched aToken transfer event."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.COLLATERAL_TRANSFER


class EnrichedDebtTransferEvent(TransferEvent):
    """Enriched vToken transfer event."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.DEBT_TRANSFER


class EnrichedDebtMintEvent(IndexScaledEvent):
    """Enriched vToken mint event (borrow)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.DEBT_MINT
    caller_address: ChecksumAddress | None = Field(
        default=None, description="Address that initiated the borrow"
    )


class EnrichedDebtBurnEvent(IndexScaledEvent):
    """Enriched vToken burn event (repay)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.DEBT_BURN
    from_address: ChecksumAddress
    target_address: ChecksumAddress | None = None


# GHO-specific events with discount handling


class EnrichedGhoDebtMintEvent(IndexScaledEvent):
    """
    Enriched GHO vToken mint event.

    GHO operations include discount calculations that affect the effective
    interest rate. The scaled amount is calculated before discount is applied.
    """

    event_type: ScaledTokenEventType = ScaledTokenEventType.GHO_DEBT_MINT
    caller_address: ChecksumAddress | None = None

    # GHO-specific fields
    discount_percent: int = Field(
        description="Discount percent in effect at time of operation (0-10000)"
    )
    discount_scaled: int = Field(description="Scaled discount amount deducted from interest")

    @field_validator("discount_percent")
    @classmethod
    def validate_discount_percent(cls, v: int) -> int:
        if not 0 <= v <= 10000:  # noqa:PLR2004
            msg = f"Discount percent must be 0-10000, got {v}"
            raise ValueError(msg)
        return v


class EnrichedGhoDebtBurnEvent(IndexScaledEvent):
    """
    Enriched GHO vToken burn event.

    GHO repayments also apply discounts when calculating the effective
    debt reduction.
    """

    event_type: ScaledTokenEventType = ScaledTokenEventType.GHO_DEBT_BURN
    from_address: ChecksumAddress
    target_address: ChecksumAddress | None = None

    # GHO-specific fields
    discount_percent: int = Field(
        description="Discount percent in effect at time of operation (0-10000)"
    )
    discount_scaled: int = Field(description="Scaled discount amount deducted from interest")

    @field_validator("discount_percent")
    @classmethod
    def validate_discount_percent(cls, v: int) -> int:
        if not 0 <= v <= 10000:  # noqa:PLR2004
            msg = f"Discount percent must be 0-10000, got {v}"
            raise ValueError(msg)
        return v


class EnrichedGhoDebtTransferEvent(TransferEvent):
    """Enriched GHO vToken transfer event (discount transfers)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.GHO_DEBT_TRANSFER
    discount_scaled: int = Field(description="Scaled discount amount being transferred")


# Interest accrual events (no Pool event, no TokenMath validation)


class EnrichedCollateralInterestMintEvent(InterestAccrualEvent):
    """Enriched aToken interest mint event (collateral interest accrual)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.COLLATERAL_INTEREST_MINT


class EnrichedCollateralInterestBurnEvent(InterestAccrualEvent):
    """Enriched aToken interest burn event (collateral interest during transfers)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.COLLATERAL_INTEREST_BURN


class EnrichedDebtInterestMintEvent(InterestAccrualEvent):
    """Enriched vToken interest mint event (debt interest accrual)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.DEBT_INTEREST_MINT


class EnrichedDebtInterestBurnEvent(InterestAccrualEvent):
    """Enriched vToken interest burn event (debt interest during transfers)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.DEBT_INTEREST_BURN


class EnrichedGhoDebtInterestMintEvent(InterestAccrualEvent):
    """Enriched GHO vToken interest mint event (GHO debt interest accrual)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.GHO_DEBT_INTEREST_MINT

    # GHO-specific fields
    discount_percent: int = Field(
        description="Discount percent in effect at time of operation (0-10000)"
    )
    discount_scaled: int = Field(description="Scaled discount amount deducted from interest")

    @field_validator("discount_percent")
    @classmethod
    def validate_discount_percent(cls, v: int) -> int:
        if not 0 <= v <= 10000:  # noqa:PLR2004
            msg = f"Discount percent must be 0-10000, got {v}"
            raise ValueError(msg)
        return v


class EnrichedGhoDebtInterestBurnEvent(InterestAccrualEvent):
    """Enriched GHO vToken interest burn event (GHO debt interest during transfers)."""

    event_type: ScaledTokenEventType = ScaledTokenEventType.GHO_DEBT_INTEREST_BURN

    # GHO-specific fields
    discount_percent: int = Field(
        description="Discount percent in effect at time of operation (0-10000)"
    )
    discount_scaled: int = Field(description="Scaled discount amount deducted from interest")

    @field_validator("discount_percent")
    @classmethod
    def validate_discount_percent(cls, v: int) -> int:
        if not 0 <= v <= 10000:  # noqa:PLR2004
            msg = f"Discount percent must be 0-10000, got {v}"
            raise ValueError(msg)
        return v


# Type union for all enriched events
EnrichedScaledTokenEvent = (
    EnrichedCollateralMintEvent
    | EnrichedCollateralBurnEvent
    | EnrichedCollateralTransferEvent
    | EnrichedDebtMintEvent
    | EnrichedDebtBurnEvent
    | EnrichedDebtTransferEvent
    | EnrichedGhoDebtMintEvent
    | EnrichedGhoDebtBurnEvent
    | EnrichedGhoDebtTransferEvent
    | EnrichedCollateralInterestMintEvent
    | EnrichedCollateralInterestBurnEvent
    | EnrichedDebtInterestMintEvent
    | EnrichedDebtInterestBurnEvent
    | EnrichedGhoDebtInterestMintEvent
    | EnrichedGhoDebtInterestBurnEvent
)
