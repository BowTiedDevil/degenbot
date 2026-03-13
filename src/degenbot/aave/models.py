"""Pydantic models for enriched Aave token events."""

from typing import Annotated, Literal

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


def _get_token_math_method(event_type: str) -> str:
    """Map event type to TokenMath method name."""
    mapping = {
        ScaledTokenEventType.COLLATERAL_MINT.value: "get_collateral_mint_scaled_amount",
        ScaledTokenEventType.COLLATERAL_BURN.value: "get_collateral_burn_scaled_amount",
        ScaledTokenEventType.COLLATERAL_TRANSFER.value: "get_collateral_transfer_scaled_amount",
        ScaledTokenEventType.DEBT_MINT.value: "get_debt_mint_scaled_amount",
        ScaledTokenEventType.DEBT_BURN.value: "get_debt_burn_scaled_amount",
        ScaledTokenEventType.GHO_DEBT_MINT.value: "get_debt_mint_scaled_amount",
        ScaledTokenEventType.GHO_DEBT_BURN.value: "get_debt_burn_scaled_amount",
    }
    if event_type not in mapping:
        msg = f"Unknown event type: {event_type}"
        raise EnrichmentError(msg)
    return mapping[event_type]


class BaseEnrichedScaledTokenEvent(BaseModel):
    """
    Base class for enriched scaled token events.

    Contains common fields without validation. Subclasses should add
    specific validation for their event types.
    """

    model_config = {"frozen": True}

    # Core identity
    event: LogReceiptField
    event_type: str
    user_address: ChecksumAddress

    # Operation context
    pool_revision: int = Field(description="Pool contract revision at time of operation")
    token_revision: int = Field(description="Token contract revision")

    # Amounts - both always present post-enrichment
    raw_amount: int = Field(description="Amount from Pool event (user input before scaling)")
    scaled_amount: int = Field(description="Scaled amount calculated by Pool using TokenMath")

    # Token contract addresses for debugging
    token_address: ChecksumAddress = Field(description="Address of the scaled token contract")
    underlying_asset: ChecksumAddress = Field(description="Address of the underlying asset")

    @property
    def is_collateral(self) -> bool:
        return self.event_type.startswith("collateral")

    @property
    def is_debt(self) -> bool:
        return self.event_type.startswith("debt") or self.event_type.startswith("gho")

    @property
    def is_burn(self) -> bool:
        return self.event_type.endswith("burn")

    @property
    def is_mint(self) -> bool:
        return self.event_type.endswith("mint")

    @property
    def is_transfer(self) -> bool:
        return self.event_type.endswith("transfer")


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

        # Calculate expected scaled amount
        token_math = TokenMathFactory.get_token_math(pool_rev)
        method_name = _get_token_math_method(event_type)
        method = getattr(token_math, method_name)

        expected = method(raw, idx)

        if scaled != expected:
            raise ScaledAmountValidationError(
                message="Scaled amount validation failed",
                event_type=event_type,
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

    event_type: Literal["collateral_mint"] = "collateral_mint"
    caller_address: ChecksumAddress | None = Field(
        default=None, description="Address that initiated the mint (may differ from user)"
    )


class EnrichedCollateralBurnEvent(IndexScaledEvent):
    """Enriched aToken burn event (withdraw)."""

    event_type: Literal["collateral_burn"] = "collateral_burn"
    from_address: ChecksumAddress
    target_address: ChecksumAddress | None = Field(
        default=None, description="Address receiving underlying asset"
    )


class EnrichedCollateralTransferEvent(TransferEvent):
    """Enriched aToken transfer event."""

    event_type: Literal["collateral_transfer"] = "collateral_transfer"


class EnrichedDebtTransferEvent(TransferEvent):
    """Enriched vToken transfer event."""

    event_type: Literal["debt_transfer"] = "debt_transfer"


class EnrichedDebtMintEvent(IndexScaledEvent):
    """Enriched vToken mint event (borrow)."""

    event_type: Literal["debt_mint"] = "debt_mint"
    caller_address: ChecksumAddress | None = Field(
        default=None, description="Address that initiated the borrow"
    )


class EnrichedDebtBurnEvent(IndexScaledEvent):
    """Enriched vToken burn event (repay)."""

    event_type: Literal["debt_burn"] = "debt_burn"
    from_address: ChecksumAddress
    target_address: ChecksumAddress | None = None


# GHO-specific events with discount handling


class EnrichedGhoDebtMintEvent(IndexScaledEvent):
    """
    Enriched GHO vToken mint event.

    GHO operations include discount calculations that affect the effective
    interest rate. The scaled amount is calculated before discount is applied.
    """

    event_type: Literal["gho_debt_mint"] = "gho_debt_mint"
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

    event_type: Literal["gho_debt_burn"] = "gho_debt_burn"
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

    event_type: Literal["gho_debt_transfer"] = "gho_debt_transfer"
    discount_scaled: int = Field(description="Scaled discount amount being transferred")


# Interest accrual events (no Pool event, no TokenMath validation)


class EnrichedCollateralInterestMintEvent(InterestAccrualEvent):
    """Enriched aToken interest mint event (collateral interest accrual)."""

    event_type: Literal["collateral_interest_mint"] = "collateral_interest_mint"


class EnrichedCollateralInterestBurnEvent(InterestAccrualEvent):
    """Enriched aToken interest burn event (collateral interest during transfers)."""

    event_type: Literal["collateral_interest_burn"] = "collateral_interest_burn"


class EnrichedDebtInterestMintEvent(InterestAccrualEvent):
    """Enriched vToken interest mint event (debt interest accrual)."""

    event_type: Literal["debt_interest_mint"] = "debt_interest_mint"


class EnrichedDebtInterestBurnEvent(InterestAccrualEvent):
    """Enriched vToken interest burn event (debt interest during transfers)."""

    event_type: Literal["debt_interest_burn"] = "debt_interest_burn"


class EnrichedGhoDebtInterestMintEvent(InterestAccrualEvent):
    """Enriched GHO vToken interest mint event (GHO debt interest accrual)."""

    event_type: Literal["gho_debt_interest_mint"] = "gho_debt_interest_mint"

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

    event_type: Literal["gho_debt_interest_burn"] = "gho_debt_interest_burn"

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
