"""Unified enriched Aave token event model.

Replaces the 18 specific Pydantic event classes with a single
EnrichedScaledTokenEvent class. The event_type field (ScaledTokenEventType
enum) fully describes each event's category, direction, and accrual type.
"""

from typing import Annotated

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
    """Raised when scaled amount enrichment fails."""


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
        """Initialize the instance."""
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
    """Map event type to TokenMath method name.

    Returns:
        The computed value.
    .

    Raises:
        EnrichmentError: If the operation fails.

    """
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


# --- Property sets for category/direction/accrual classification ---

_COLLATERAL_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.COLLATERAL_MINT,
    ScaledTokenEventType.COLLATERAL_BURN,
    ScaledTokenEventType.COLLATERAL_TRANSFER,
    ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
    ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
}

_DEBT_TYPES: set[ScaledTokenEventType] = {
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

_BURN_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.COLLATERAL_BURN,
    ScaledTokenEventType.DEBT_BURN,
    ScaledTokenEventType.GHO_DEBT_BURN,
    ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
    ScaledTokenEventType.DEBT_INTEREST_BURN,
    ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
}

_MINT_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.COLLATERAL_MINT,
    ScaledTokenEventType.DEBT_MINT,
    ScaledTokenEventType.GHO_DEBT_MINT,
    ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
    ScaledTokenEventType.DEBT_INTEREST_MINT,
    ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
}

_TRANSFER_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.COLLATERAL_TRANSFER,
    ScaledTokenEventType.DEBT_TRANSFER,
    ScaledTokenEventType.GHO_DEBT_TRANSFER,
}

_GHO_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.GHO_DEBT_MINT,
    ScaledTokenEventType.GHO_DEBT_BURN,
    ScaledTokenEventType.GHO_DEBT_TRANSFER,
    ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
    ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
}

_INTEREST_TYPES: set[ScaledTokenEventType] = {
    ScaledTokenEventType.COLLATERAL_INTEREST_MINT,
    ScaledTokenEventType.COLLATERAL_INTEREST_BURN,
    ScaledTokenEventType.DEBT_INTEREST_MINT,
    ScaledTokenEventType.DEBT_INTEREST_BURN,
    ScaledTokenEventType.GHO_DEBT_INTEREST_MINT,
    ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
}

_INDEX_SCALED_TYPES: set[ScaledTokenEventType] = _MINT_TYPES | _BURN_TYPES | _INTEREST_TYPES


class EnrichedScaledTokenEvent(BaseModel):
    """Unified enriched scaled token event.

    Replaces the previous 18 specific event classes. The event_type
    field (ScaledTokenEventType enum) fully describes each event's
    category, direction, and accrual type. Optional fields are present
    based on the event type:

    - index/balance_increase: required for mint/burn/interest events
    - from_address/to_address: required for transfer events
    - caller_address: present for mint events
    - target_address: present for burn events
    - discount_percent/discount_scaled: required for GHO events
    """

    model_config = {"frozen": True}

    # Core identity
    event: LogReceiptField
    event_type: ScaledTokenEventType
    user_address: ChecksumAddress

    # Operation context
    pool_revision: int = Field(description="Pool contract revision at time of operation")
    token_revision: int = Field(description="Token contract revision")

    # Amounts
    raw_amount: int = Field(description="Amount from Pool event (user input before scaling)")
    scaled_amount: int | None = Field(
        default=None,
        description="Scaled amount calculated by Pool using TokenMath",
    )

    # Token contract addresses
    token_address: ChecksumAddress = Field(description="Address of the scaled token contract")
    underlying_asset: ChecksumAddress = Field(description="Address of the underlying asset")

    # Index-scaled fields (present for mint/burn/interest events)
    index: int | None = Field(
        default=None,
        description="Current liquidity/borrow index used for scaling",
    )
    balance_increase: int | None = Field(
        default=None,
        description="Interest accrued since last interaction",
    )

    # Transfer fields (present for transfer events)
    from_address: ChecksumAddress | None = Field(
        default=None,
        description="Sender address for burn/transfer events",
    )
    to_address: ChecksumAddress | None = Field(
        default=None,
        description="Recipient address for transfer events",
    )

    # Mint/burn-specific fields
    caller_address: ChecksumAddress | None = Field(
        default=None,
        description="Address that initiated the mint (may differ from user)",
    )
    target_address: ChecksumAddress | None = Field(
        default=None,
        description="Address receiving underlying asset (burn) or target (transfer)",
    )

    # GHO-specific fields (present for GHO events)
    discount_percent: int | None = Field(
        default=None,
        description="Discount percent in effect at time of operation (0-10000)",
    )
    discount_scaled: int | None = Field(
        default=None,
        description="Scaled discount amount deducted from interest",
    )

    @property
    def is_collateral(self) -> bool:
        """Check if collateral."""
        return self.event_type in _COLLATERAL_TYPES

    @property
    def is_debt(self) -> bool:
        """Check if debt."""
        return self.event_type in _DEBT_TYPES

    @property
    def is_burn(self) -> bool:
        """Check if burn."""
        return self.event_type in _BURN_TYPES

    @property
    def is_mint(self) -> bool:
        """Check if mint."""
        return self.event_type in _MINT_TYPES

    @property
    def is_transfer(self) -> bool:
        """Check if transfer."""
        return self.event_type in _TRANSFER_TYPES

    @property
    def is_gho(self) -> bool:
        """Check if gho."""
        return self.event_type in _GHO_TYPES

    @property
    def is_interest(self) -> bool:
        """Check if interest."""
        return self.event_type in _INTEREST_TYPES

    @field_validator("discount_percent")
    @classmethod
    def validate_discount_percent(cls, v: int | None) -> int | None:
        """Validate discount percent.

        Returns:
            The computed value.
        .

        Raises:
            ValueError: If the operation fails.

        """
        if v is not None and not 0 <= v <= 10000:  # noqa:PLR2004
            msg = f"discount_percent must be 0-10000, got {v}"
            raise ValueError(msg)
        return v

    @model_validator(mode="after")
    def validate_scaled_amount(self) -> "EnrichedScaledTokenEvent":
        """Validate scaled amount using TokenMath for index-scaled events.

        Returns:
            The computed value.
        .

        Raises:
            ScaledAmountValidationError: If the operation fails.

        """
        # Interest accrual events: no TokenMath validation
        if self.event_type in _INTEREST_TYPES:
            return self

        # Transfer events: no index-based validation
        if self.event_type in _TRANSFER_TYPES:
            return self

        # scaled_amount=None means skip validation (enrichment override)
        if self.scaled_amount is None:
            return self

        # Only index-scaled mint/burn events need TokenMath validation
        if self.event_type not in _INDEX_SCALED_TYPES:
            return self

        pool_rev = self.pool_revision
        token_rev = self.token_revision
        raw = self.raw_amount
        scaled = self.scaled_amount

        assert self.index is not None  # guaranteed for index-scaled events
        idx = self.index

        # Pool Revision 9+ LIQUIDATION debt amounts are pre-scaled
        if pool_rev >= 9 and self.event_type == ScaledTokenEventType.DEBT_BURN:  # noqa: PLR2004
            return self

        # Calculate expected scaled amount
        token_math = TokenMathFactory.get_token_math_for_token_revision(token_rev)
        method_name = _get_token_math_method(self.event_type)
        method = getattr(token_math, method_name)
        expected = method(raw, idx)

        # Withdraw with interest exceeding withdrawal amount
        # The Pool uses burn rounding (ceil) but emits a Mint event.
        if (
            self.event_type == ScaledTokenEventType.COLLATERAL_MINT
            and self.balance_increase is not None
            and raw < self.balance_increase
        ):
            expected_burn = token_math.get_collateral_burn_scaled_amount(
                amount=raw, liquidity_index=idx
            )
            if scaled == expected_burn:
                return self

        # REPAY with interest exceeding repayment amount
        # The Pool uses burn rounding (floor) but emits a Mint event.
        if (
            self.event_type in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
            and self.balance_increase is not None
            and self.balance_increase > 0
        ):
            expected_burn = token_math.get_debt_burn_scaled_amount(amount=raw, borrow_index=idx)
            if scaled == expected_burn:
                return self

        if scaled != expected:
            raise ScaledAmountValidationError(
                message="Scaled amount validation failed",
                event_type=str(self.event_type),
                pool_revision=pool_rev,
                token_revision=token_rev,
                raw_amount=raw,
                expected_scaled=expected,
                actual_scaled=scaled,
                index=idx,
                token_math_method=method_name,
            )

        return self
