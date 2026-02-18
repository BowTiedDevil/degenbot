"""Base protocols and types for Aave token processors."""

from dataclasses import dataclass
from typing import ClassVar, Protocol, TypedDict

from eth_typing import ChecksumAddress


class PercentageMathLibrary(Protocol):
    """Protocol for percentage math operations."""

    def percent_div(self, value: int, percentage: int) -> int: ...
    def percent_mul(self, value: int, percentage: int) -> int: ...


class WadRayMathLibrary(Protocol):
    """Protocol for Wad/Ray math operations."""

    def ray_div(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...


class MathLibraries(TypedDict):
    """Container for math library modules."""

    wad_ray: WadRayMathLibrary
    percentage: PercentageMathLibrary


@dataclass(frozen=True, slots=True)
class CollateralMintEvent:
    """Data for collateral (aToken) mint events."""

    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class CollateralBurnEvent:
    """Data for collateral (aToken) burn events."""

    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class DebtMintEvent:
    """Data for debt (vToken) mint events."""

    caller: ChecksumAddress
    on_behalf_of: ChecksumAddress
    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class DebtBurnEvent:
    """Data for debt (vToken) burn events."""

    from_: ChecksumAddress
    target: ChecksumAddress
    value: int
    balance_increase: int
    index: int


class ProcessingResult(Protocol):
    """Protocol for processor results with balance delta and new index."""

    balance_delta: int
    new_index: int


@dataclass(frozen=True, slots=True)
class MintResult:
    """Result of processing a mint event (collateral or standard debt)."""

    balance_delta: int
    new_index: int
    is_repay: bool  # True for repay/withdrawal, False for deposit/borrow


@dataclass(frozen=True, slots=True)
class BurnResult:
    """Result of processing a burn event (collateral or standard debt)."""

    balance_delta: int
    new_index: int


@dataclass(frozen=True, slots=True)
class GhoMintResult:
    """Result of processing a GHO debt mint event."""

    balance_delta: int
    new_index: int
    user_operation: str  # "GHO BORROW", "GHO REPAY", or "GHO INTEREST ACCRUAL"
    discount_scaled: int
    should_refresh_discount: bool


@dataclass(frozen=True, slots=True)
class GhoBurnResult:
    """Result of processing a GHO debt burn event."""

    balance_delta: int
    new_index: int
    discount_scaled: int
    should_refresh_discount: bool


class TokenProcessor(Protocol):
    """Base protocol for all token processors."""

    revision: ClassVar[int]

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        ...


class CollateralTokenProcessor(TokenProcessor, Protocol):
    """Protocol for collateral (aToken) processors.

    Processors are stateless - they calculate deltas and return results
    without modifying position state. Callers must apply the results.
    """

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        previous_balance: int,
        previous_index: int,
        scaled_delta: int | None = None,
    ) -> MintResult:
        """
        Process a collateral mint event.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Optional pre-calculated scaled amount delta

        Returns:
            MintResult with balance_delta, new_index, and is_repay flag
        """
        ...

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        previous_balance: int,
        previous_index: int,
    ) -> BurnResult:
        """
        Process a collateral burn event.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            BurnResult with balance_delta and new_index
        """
        ...

    def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount from raw underlying amount.

        Args:
            raw_amount: The raw underlying token amount
            index: The current liquidity index

        Returns:
            The scaled amount
        """
        ...


class DebtTokenProcessor(TokenProcessor, Protocol):
    """Protocol for standard debt (vToken) processors.

    This protocol is for non-GHO variable debt tokens.
    GHO tokens have special discount handling and use GhoDebtTokenProcessor instead.

    Processors are stateless - they calculate deltas and return results
    without modifying position state. Callers must apply the results.
    """

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,
        previous_index: int,
    ) -> MintResult:
        """
        Process a debt mint event.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            MintResult with balance_delta, new_index, and is_repay flag
        """
        ...

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,
        previous_index: int,
    ) -> BurnResult:
        """
        Process a debt burn event.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            BurnResult with balance_delta and new_index
        """
        ...


class GhoDebtTokenProcessor(TokenProcessor, Protocol):
    """Protocol for GHO variable debt token processors.

    GHO debt tokens have special discount handling that requires additional
    parameters and return values compared to standard vTokens.

    Processors are stateless - they calculate deltas and return results
    without modifying position state. Callers must apply the results.
    """

    def supports_discount(self) -> bool:
        """Check if this revision supports the discount mechanism."""
        ...

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
    ) -> GhoMintResult:
        """
        Process a GHO debt mint event.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: The discount percent before this transaction

        Returns:
            GhoMintResult with balance_delta, new_index, is_repay,
            discount_scaled, and should_refresh_discount flag
        """
        ...

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
    ) -> GhoBurnResult:
        """
        Process a GHO debt burn event.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: The discount percent before this transaction

        Returns:
            GhoBurnResult with balance_delta, new_index, discount_scaled,
            and should_refresh_discount flag
        """
        ...

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,
        current_index: int,
        discount_percent: int,
    ) -> int:
        """
        Calculate discounted balance for burn operations.

        Args:
            scaled_balance: The scaled balance
            previous_index: The previous index from user state
            current_index: The current debt index
            discount_percent: The discount percentage to apply

        Returns:
            The balance with discount applied
        """
        ...

    def accrue_debt_on_action(
        self,
        previous_scaled_balance: int,
        previous_index: int,
        discount_percent: int,
        current_index: int,
    ) -> int:
        """
        Calculate debt accrual with discount.

        Simulates the _accrueDebtOnAction function from the contract.
        This is a stateless calculation - it returns the discount_scaled
        amount without modifying any position state.

        Args:
            previous_scaled_balance: Balance before the action
            previous_index: The index at previous_scaled_balance calculation
            discount_percent: Current discount percentage
            current_index: Current variable debt index

        Returns:
            The discount scaled amount (0 for revisions without discount support)
        """
        ...
