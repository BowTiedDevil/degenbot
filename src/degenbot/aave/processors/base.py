"""Base protocols and types for Aave token processors."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, ClassVar, Protocol, TypedDict

from eth_typing import ChecksumAddress

if TYPE_CHECKING:
    from degenbot.database.models.aave import (
        AaveV3CollateralPositionsTable,
        AaveV3DebtPositionsTable,
    )


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


class TokenProcessor(Protocol):
    """Base protocol for all token processors."""

    revision: ClassVar[int]

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        ...


class CollateralTokenProcessor(TokenProcessor, Protocol):
    """Protocol for collateral (aToken) processors."""

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        position: "AaveV3CollateralPositionsTable",
        scaled_delta: int | None = None,
    ) -> tuple[int, bool]:
        """
        Process a collateral mint event.

        Args:
            event_data: The mint event data
            position: The user's collateral position to update
            scaled_delta: Optional pre-calculated scaled amount delta

        Returns:
            Tuple of (balance_delta, is_withdrawal)
        """
        ...

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        position: "AaveV3CollateralPositionsTable",
    ) -> int:
        """
        Process a collateral burn event.

        Args:
            event_data: The burn event data
            position: The user's collateral position to update

        Returns:
            The balance delta (negative for withdrawal)
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
    """Protocol for debt (vToken) processors."""

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        position: "AaveV3DebtPositionsTable",
        *,
        previous_discount: int = 0,
    ) -> tuple[int, bool] | tuple[int, bool, int]:
        """
        Process a debt mint event.

        Args:
            event_data: The mint event data
            position: The user's debt position to update
            previous_discount: The discount percent before this transaction (GHO only)

        Returns:
            Tuple of (balance_delta, is_repay) for standard tokens,
            or (balance_delta, is_repay, discount_scaled) for GHO tokens
        """
        ...

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        position: "AaveV3DebtPositionsTable",
        *,
        previous_discount: int = 0,
    ) -> int | tuple[int, int]:
        """
        Process a debt burn event.

        Args:
            event_data: The burn event data
            position: The user's debt position to update
            previous_discount: The discount percent before this transaction (GHO only)

        Returns:
            The balance delta (negative for repayment) for standard tokens,
            or (balance_delta, discount_scaled) for GHO tokens
        """
        ...


class GhoTokenProcessor(DebtTokenProcessor, Protocol):
    """Protocol for GHO variable debt token processors."""

    def supports_discount(self) -> bool:
        """Check if this revision supports the discount mechanism."""
        ...

    def accrue_debt_on_action(
        self,
        position: "AaveV3DebtPositionsTable",
        previous_scaled_balance: int,
        discount_percent: int,
        index: int,
    ) -> int:
        """
        Simulate _accrueDebtOnAction function.

        Args:
            position: The user's debt position
            previous_scaled_balance: Balance before the action
            discount_percent: Current discount percentage
            index: Current variable debt index

        Returns:
            The discount scaled amount
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
