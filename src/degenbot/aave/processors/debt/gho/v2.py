"""GHO variable debt token processor for revisions 2-3."""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_2 as aave_library_v3_2
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    MathLibraries,
)
from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3DebtPositionsTable


class GhoV2Processor(GhoV1Processor):
    """Processor for GHO VariableDebtToken revisions 2-3.

    Revisions 2-3 have full discount support with getDiscountedBalance logic
    for accurate balance calculations during burn operations.
    """

    revision = 2

    def __init__(self) -> None:
        """Initialize with v3.2 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_2.wad_ray_math,
            percentage=aave_library_v3_2.percentage_math,
        )

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        position: "AaveV3DebtPositionsTable",
        previous_discount: int = 0,
    ) -> tuple[int, bool, int]:
        """
        Process a GHO debt mint event with full discount support.

        Args:
            event_data: The mint event data
            position: The user's debt position to update
            previous_discount: The discount percent before this transaction

        Returns:
            Tuple of (balance_delta, is_repay, discount_scaled)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        previous_scaled_balance = position.balance
        previous_index = position.last_index or 0

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

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            requested_amount = event_data.balance_increase - event_data.value
            amount_scaled = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # Get balance before burn with discount
            balance_before_burn = self.get_discounted_balance(
                scaled_balance=previous_scaled_balance,
                previous_index=previous_index,
                current_index=event_data.index,
                discount_percent=previous_discount,
            )

            if requested_amount == balance_before_burn:
                # Full repayment: burn all scaled balance
                balance_delta = -previous_scaled_balance
            else:
                # Partial repayment
                balance_delta = -(amount_scaled + discount_scaled)

            is_repay = True

        else:
            # Pure interest accrual (value == balance_increase)
            balance_delta = -discount_scaled
            is_repay = False

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
        Process a GHO debt burn event with full discount support.

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
        previous_index = position.last_index or 0

        # Get balance before burn with discount
        balance_before_burn = self.get_discounted_balance(
            scaled_balance=previous_scaled_balance,
            previous_index=previous_index,
            current_index=event_data.index,
            discount_percent=previous_discount,
        )

        # Accrue debt with discount
        discount_scaled = self.accrue_debt_on_action(
            position=position,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=previous_discount,
            index=event_data.index,
        )

        if requested_amount == balance_before_burn:
            # Full repayment: burn all scaled balance
            balance_delta = -previous_scaled_balance
        else:
            # Partial repayment
            balance_delta = -(amount_scaled + discount_scaled)

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta, discount_scaled

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,
        current_index: int,
        discount_percent: int,
    ) -> int:
        """
        Calculate discounted balance for burn operations.

        This replicates the super.balanceOf(user) call with discount applied.

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
