"""GHO variable debt token processor for revisions 2-3."""

import degenbot.aave.libraries.v3_2 as aave_library_v3_2
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    GhoBurnResult,
    GhoMintResult,
    MathLibraries,
)
from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor


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
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
    ) -> GhoMintResult:
        """
        Process a GHO debt mint event with full discount support.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: The discount percent before this transaction

        Returns:
            GhoMintResult with balance_delta, new_index, user_operation,
            discount_scaled, and should_refresh_discount
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # Accrue debt with discount (stateless - uses inherited method)
        discount_scaled = self.accrue_debt_on_action(
            previous_scaled_balance=previous_balance,
            previous_index=previous_index,
            discount_percent=previous_discount,
            current_index=event_data.index,
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

            user_operation = "GHO BORROW"

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            requested_amount = event_data.balance_increase - event_data.value
            amount_scaled = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # Get balance before burn with discount
            balance_before_burn = self.get_discounted_balance(
                scaled_balance=previous_balance,
                previous_index=previous_index,
                current_index=event_data.index,
                discount_percent=previous_discount,
            )

            if requested_amount == balance_before_burn:
                # Full repayment: burn all scaled balance
                balance_delta = -previous_balance
            else:
                # Partial repayment
                balance_delta = -(amount_scaled + discount_scaled)

            user_operation = "GHO REPAY"

        else:
            # Pure interest accrual (value == balance_increase)
            # Emitted from _accrueDebtOnAction during discount updates
            # The balance decreases by the discount amount (burned by contract)
            balance_delta = -discount_scaled
            user_operation = "GHO INTEREST ACCRUAL"

        # For GHO rev 2-3, always refresh discount after balance-changing operations
        should_refresh_discount = True

        return GhoMintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            user_operation=user_operation,
            discount_scaled=discount_scaled,
            should_refresh_discount=should_refresh_discount,
        )

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
    ) -> GhoBurnResult:
        """
        Process a GHO debt burn event with full discount support.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: The discount percent before this transaction

        Returns:
            GhoBurnResult with balance_delta, new_index, discount_scaled,
            and should_refresh_discount
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = wad_ray_math.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # Get balance before burn with discount
        balance_before_burn = self.get_discounted_balance(
            scaled_balance=previous_balance,
            previous_index=previous_index,
            current_index=event_data.index,
            discount_percent=previous_discount,
        )

        # Accrue debt with discount (stateless - uses inherited method)
        discount_scaled = self.accrue_debt_on_action(
            previous_scaled_balance=previous_balance,
            previous_index=previous_index,
            discount_percent=previous_discount,
            current_index=event_data.index,
        )

        if requested_amount == balance_before_burn:
            # Full repayment: burn all scaled balance
            balance_delta = -previous_balance
        else:
            # Partial repayment
            balance_delta = -(amount_scaled + discount_scaled)

        # For GHO rev 2-3, always refresh discount after balance-changing operations
        should_refresh_discount = True

        return GhoBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            discount_scaled=discount_scaled,
            should_refresh_discount=should_refresh_discount,
        )

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
