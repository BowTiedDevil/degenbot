"""
GHO variable debt token processor for revisions 5+.

Revisions 5+ deprecate the discount mechanism and use explicit floor/ceil
division via the TokenMath library.
"""

import typing

import degenbot.aave.libraries as aave_library_v3_5
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    GhoBurnResult,
    GhoDebtTokenProcessor,
    GhoMintResult,
    GhoUserOperation,
    MathLibraries,
)


class GhoV5Processor(GhoDebtTokenProcessor):
    """Processor for GHO VariableDebtToken revisions 5+.

    Revisions 5+ have the discount mechanism deprecated and use explicit
    floor (ray_div_floor) and ceiling (ray_div_ceil) division functions
    from the TokenMath library.
    """

    revision = 5
    math_lib_version = "v3.5"

    def __init__(self) -> None:
        """Initialize with v3.5 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_5.wad_ray_math,
            percentage=aave_library_v3_5.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def supports_discount(self) -> bool:  # noqa: PLR6301
        """Revision 5+ does not support discount mechanism."""
        return False

    def calculate_mint_scaled_amount(self, raw_amount: int, index: int) -> int:
        """Calculate scaled amount from raw underlying amount for mint operations.

        GHO V5+ uses ceiling division (ray_div_ceil) for mint operations to match
        TokenMath.getVTokenMintScaledAmount behavior in the Pool contract.

        This method should be called before process_mint_event() to pre-calculate
        the scaled_delta parameter from the original borrow amount, ensuring
        consistent rounding with on-chain calculations.

        Args:
            raw_amount: The raw underlying amount (unscaled)
            index: The variable debt index

        Returns:
            The scaled amount (amountScaled)
        """
        return self._math_libs["wad_ray"].ray_div_ceil(
            a=raw_amount,
            b=index,
        )

    def calculate_burn_scaled_amount(self, raw_amount: int, index: int) -> int:
        """Calculate scaled amount from raw underlying amount for burn operations.

        GHO V5+ uses floor division (ray_div_floor) for burn operations to match
        TokenMath.getVTokenBurnScaledAmount behavior in the Pool contract.

        This method should be called before process_burn_event() to pre-calculate
        the scaled_delta parameter from the original payback amount, ensuring
        consistent rounding with on-chain calculations.

        Args:
            raw_amount: The raw underlying amount (unscaled)
            index: The variable debt index

        Returns:
            The scaled amount (amountScaled)
        """
        return self._math_libs["wad_ray"].ray_div_floor(
            a=raw_amount,
            b=index,
        )

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,  # noqa: ARG002
    ) -> GhoMintResult:
        """Process a GHO debt mint event without discount.

        For accurate balance tracking, the scaled_delta parameter should be
        pre-calculated using calculate_mint_scaled_amount() from the original
        borrow amount (extracted from the BORROW event). This ensures the
        rounding matches the on-chain TokenMath.getVTokenMintScaledAmount
        calculation which uses ceiling division.

        Args:
            event_data: The mint event data from the token contract
            previous_balance: The user's scaled balance before this event
            previous_index: The index at previous_balance calculation
            previous_discount: Ignored (no discount in rev 5+)

        Returns:
            GhoMintResult with balance_delta, new_index, user_operation,
            discount_scaled=0, and should_refresh_discount=False
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.value > event_data.balance_increase:
            # GHO BORROW: emitted in _mintScaled
            #
            # Revision 5+ uses ceiling division (ray_div_ceil) for BORROW
            # to match TokenMath.getVTokenMintScaledAmount behavior.
            # This ensures the protocol never underaccounts the user's debt.
            #
            # When processing BORROW events, use scaled_amount from Pool contract
            # if available to ensure exact matching with on-chain calculations.
            if event_data.scaled_amount is not None:
                balance_delta = event_data.scaled_amount
            else:
                requested_amount = event_data.value - event_data.balance_increase
                balance_delta = wad_ray_math.ray_div_ceil(
                    a=requested_amount,
                    b=event_data.index,
                )
            user_operation = GhoUserOperation.GHO_BORROW

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            #
            # Revision 5+ uses floor division (ray_div_floor) for REPAY
            # to match TokenMath.getVTokenBurnScaledAmount behavior.
            # This prevents over-burning of vTokens.
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div_floor(
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

        # Revision 5+ never refreshes discount (discount is deprecated)
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
            previous_discount: Ignored (no discount in rev 5+)

        Returns:
            GhoBurnResult with balance_delta, new_index, discount_scaled=0,
            and should_refresh_discount=False
        """
        # Use pre-calculated scaled amount from Pool contract if available
        if event_data.scaled_amount is not None:
            return GhoBurnResult(
                balance_delta=-event_data.scaled_amount,
                new_index=event_data.index,
                discount_scaled=0,
                should_refresh_discount=False,
            )

        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease
        requested_amount = event_data.value + event_data.balance_increase

        # Revision 5+ uses floor division (ray_div_floor) via TokenMath.getVTokenBurnScaledAmount
        # to prevent over-burning of vTokens.
        # No discount in rev 5+
        balance_delta = -wad_ray_math.ray_div_floor(
            a=requested_amount,
            b=event_data.index,
        )

        # Revision 5+ never refreshes discount (discount is deprecated)
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

        In revision 5+, this simply returns rayMul(scaled_balance, current_index).

        Args:
            scaled_balance: The scaled balance
            previous_index: Ignored
            current_index: The current debt index
            discount_percent: Ignored (no discount in rev 5+)

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

        In revision 5+, the discount mechanism is deprecated, so this
        always returns 0.

        Args:
            previous_scaled_balance: Ignored
            previous_index: Ignored
            discount_percent: Ignored (no discount in rev 5+)
            current_index: Ignored

        Returns:
            Always returns 0
        """
        return 0
