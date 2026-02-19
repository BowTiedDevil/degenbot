"""GHO variable debt token processor for revisions 4+.

Revision 4+ deprecates the discount mechanism entirely.
"""

import typing

import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    GhoBurnResult,
    GhoDebtTokenProcessor,
    GhoMintResult,
    GhoUserOperation,
    MathLibraries,
)


class GhoV4Processor(GhoDebtTokenProcessor):
    """Processor for GHO VariableDebtToken revisions 4+.

    Revisions 4+ have the discount mechanism deprecated.
    """

    revision = 4
    math_lib_version = "v3.4"

    def __init__(self) -> None:
        """Initialize with v3.4 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_4.wad_ray_math,
            percentage=aave_library_v3_4.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def supports_discount(self) -> bool:  # noqa: PLR6301
        """Revision 4+ does not support discount mechanism."""
        return False

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,  # noqa: ARG002
    ) -> GhoMintResult:
        """Process a GHO debt mint event without discount.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: Ignored (no discount in rev 4+)

        Returns:
            GhoMintResult with balance_delta, new_index, user_operation,
            discount_scaled=0, and should_refresh_discount=False
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.value > event_data.balance_increase:
            # GHO BORROW: emitted in _mintScaled
            # Revision 4+ uses ceiling division (ray_div_ceil) to match
            # TokenMath.getVTokenMintScaledAmount behavior.
            requested_amount = event_data.value - event_data.balance_increase
            balance_delta = wad_ray_math.ray_div_ceil(
                a=requested_amount,
                b=event_data.index,
            )
            user_operation = GhoUserOperation.GHO_BORROW

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            user_operation = GhoUserOperation.GHO_REPAY

        else:
            # Pure interest accrual (value == balance_increase)
            balance_increase = wad_ray_math.ray_mul(
                a=previous_balance,
                b=event_data.index,
            ) - wad_ray_math.ray_mul(
                a=previous_balance,
                b=previous_index,
            )

            # Convert back to scaled
            balance_increase_scaled = wad_ray_math.ray_div(
                a=balance_increase,
                b=event_data.index,
            )

            balance_delta = balance_increase_scaled
            user_operation = GhoUserOperation.GHO_INTEREST_ACCRUAL

        # Revision 4+ never refreshes discount (discount is deprecated)
        should_refresh_discount = False

        return GhoMintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            user_operation=user_operation,
            discount_scaled=0,
            should_refresh_discount=should_refresh_discount,
        )

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        previous_discount: int,  # noqa: ARG002
    ) -> GhoBurnResult:
        """Process a GHO debt burn event without discount.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: Ignored (no discount in rev 4+)

        Returns:
            GhoBurnResult with balance_delta, new_index, discount_scaled=0,
            and should_refresh_discount=False
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDivFloor(index)
        # Revision 4+ uses floor division to match TokenMath.getVTokenBurnScaledAmount
        # No discount in rev 4+
        balance_delta = -wad_ray_math.ray_div_floor(
            a=requested_amount,
            b=event_data.index,
        )

        # Revision 4+ never refreshes discount (discount is deprecated)
        should_refresh_discount = False

        return GhoBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            discount_scaled=0,
            should_refresh_discount=should_refresh_discount,
        )

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,  # noqa: ARG002
        current_index: int,
        discount_percent: int,  # noqa: ARG002
    ) -> int:
        """Calculate balance without discount.

        In revision 4+, this simply returns rayMul(scaled_balance, current_index).

        Args:
            scaled_balance: The scaled balance
            previous_index: Ignored
            current_index: The current debt index
            discount_percent: Ignored (no discount in rev 4+)

        Returns:
            The balance without discount
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if scaled_balance == 0:
            return 0

        return wad_ray_math.ray_mul(
            a=scaled_balance,
            b=current_index,
        )

    @typing.override
    def accrue_debt_on_action(
        self,
        previous_scaled_balance: int,
        previous_index: int,
        discount_percent: int,
        current_index: int,
    ) -> int:
        """Calculate debt accrual without discount.

        In revision 4+, the discount mechanism is deprecated, so this
        always returns 0.

        Args:
            previous_scaled_balance: Ignored
            previous_index: Ignored
            discount_percent: Ignored (no discount in rev 4+)
            current_index: Ignored

        Returns:
            Always returns 0
        """
        return 0
