"""GHO variable debt token processor for revision 1."""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_1 as aave_library_v3_1
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    GhoTokenProcessor,
    MathLibraries,
)

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3DebtPositionsTable


class GhoV1Processor(GhoTokenProcessor):
    """Processor for GHO VariableDebtToken revision 1.

    Revision 1 has basic discount support without the getDiscountedBalance logic.
    """

    revision = 1

    def __init__(self) -> None:
        """Initialize with v3.1 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_1.wad_ray_math,
            percentage=aave_library_v3_1.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def supports_discount(self) -> bool:
        """Revision 1 supports discount mechanism."""
        return True

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        position: "AaveV3DebtPositionsTable",
        previous_discount: int = 0,
    ) -> tuple[int, bool, int]:
        """
        Process a GHO debt mint event.

        Args:
            event_data: The mint event data
            position: The user's debt position to update
            previous_discount: The discount percent before this transaction

        Returns:
            Tuple of (balance_delta, is_repay, discount_scaled)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        previous_scaled_balance = position.balance

        # Accrue debt with discount
        discount_scaled = self.accrue_debt_on_action(
            position=position,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=previous_discount,
            index=event_data.index,
        )

        if event_data.value > event_data.balance_increase:
            # GHO BORROW: emitted in _mintScaled
            requested_amount = event_data.value - event_data.balance_increase
            amount_scaled = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            if amount_scaled > discount_scaled:
                balance_delta = amount_scaled - discount_scaled
            else:
                balance_delta = -(discount_scaled - amount_scaled)

            is_repay = False
        else:
            # GHO REPAY: emitted in _burnScaled
            requested_amount = event_data.balance_increase - event_data.value
            amount_scaled = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            if amount_scaled > discount_scaled:
                balance_delta = -(amount_scaled - discount_scaled)
            else:
                balance_delta = discount_scaled - amount_scaled

            is_repay = True

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta, is_repay, discount_scaled

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        position: "AaveV3DebtPositionsTable",
        previous_discount: int = 0,
    ) -> tuple[int, int]:
        """
        Process a GHO debt burn event.

        Args:
            event_data: The burn event data
            position: The user's debt position to update
            previous_discount: The discount percent before this transaction

        Returns:
            Tuple of (balance_delta, discount_scaled)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = wad_ray_math.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        previous_scaled_balance = position.balance

        # Accrue debt with discount
        discount_scaled = self.accrue_debt_on_action(
            position=position,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=previous_discount,
            index=event_data.index,
        )

        # Solidity: _burn(user, (amountScaled + discountScaled).toUint128())
        balance_delta = -(amount_scaled + discount_scaled)

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta, discount_scaled

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
        wad_ray_math = self._math_libs["wad_ray"]
        percentage_math = self._math_libs["percentage"]

        # Calculate balance increase
        balance_increase = wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=index,
        ) - wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=position.last_index or 0,
        )

        discount_scaled = 0
        if balance_increase != 0 and discount_percent != 0:
            discount = percentage_math.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )
            discount_scaled = wad_ray_math.ray_div(a=discount, b=index)
            balance_increase -= discount

        # Update last_index to match contract behavior
        position.last_index = index

        return discount_scaled

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,
        current_index: int,
        discount_percent: int,
    ) -> int:
        """
        Calculate discounted balance for burn operations.

        For revision 1, this is simpler than later revisions.

        Args:
            scaled_balance: The scaled balance
            previous_index: The previous index from user state
            current_index: The current debt index
            discount_percent: The discount percentage to apply

        Returns:
            The balance with discount applied
        """
        wad_ray_math = self._math_libs["wad_ray"]
        percentage_math = self._math_libs["percentage"]

        if scaled_balance == 0:
            return 0

        balance = wad_ray_math.ray_mul(
            a=scaled_balance,
            b=current_index,
        )

        if current_index == previous_index:
            return balance

        if discount_percent != 0:
            balance_increase = balance - wad_ray_math.ray_mul(
                a=scaled_balance,
                b=previous_index,
            )

            balance -= percentage_math.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )

        return balance
