"""GHO variable debt token processor for revisions 4+.

Revision 4+ deprecates the discount mechanism entirely.
"""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    GhoTokenProcessor,
    MathLibraries,
)

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3DebtPositionsTable


class GhoV4Processor(GhoTokenProcessor):
    """Processor for GHO VariableDebtToken revisions 4+.

    Revisions 4+ have the discount mechanism deprecated.
    """

    revision = 4

    def __init__(self) -> None:
        """Initialize with v3.4 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_4.wad_ray_math,
            percentage=aave_library_v3_4.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def supports_discount(self) -> bool:
        """Revision 4+ does not support discount mechanism."""
        return False

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        position: "AaveV3DebtPositionsTable",
        previous_discount: int = 0,
    ) -> tuple[int, bool, int]:
        """Process a GHO debt mint event without discount.

        Args:
            event_data: The mint event data
            position: The user's debt position to update
            previous_discount: Ignored (no discount in rev 4+)

        Returns:
            Tuple of (balance_delta, is_repay, discount_scaled=0)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.value > event_data.balance_increase:
            # GHO BORROW: emitted in _mintScaled
            requested_amount = event_data.value - event_data.balance_increase
            balance_delta = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = False

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = True

        else:
            # Pure interest accrual (value == balance_increase)
            previous_scaled_balance = position.balance
            balance_increase = wad_ray_math.ray_mul(
                a=previous_scaled_balance,
                b=event_data.index,
            ) - wad_ray_math.ray_mul(
                a=previous_scaled_balance,
                b=position.last_index or 0,
            )

            # Convert back to scaled
            balance_increase_scaled = wad_ray_math.ray_div(
                a=balance_increase,
                b=event_data.index,
            )

            balance_delta = balance_increase_scaled
            is_repay = False

            # Update last_index
            position.last_index = event_data.index

        position.balance += balance_delta
        if event_data.value != event_data.balance_increase:
            # Update last_index for non-interest-accrual events
            position.last_index = event_data.index

        return balance_delta, is_repay, 0

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        position: "AaveV3DebtPositionsTable",
        previous_discount: int = 0,
    ) -> tuple[int, int]:
        """Process a GHO debt burn event without discount.

        Args:
            event_data: The burn event data
            position: The user's debt position to update
            previous_discount: Ignored (no discount in rev 4+)

        Returns:
            Tuple of (balance_delta, discount_scaled=0)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index)
        # No discount in rev 4+
        balance_delta = -wad_ray_math.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta, 0

    def accrue_debt_on_action(
        self,
        position: "AaveV3DebtPositionsTable",
        previous_scaled_balance: int,
        discount_percent: int,
        index: int,
    ) -> int:
        """Simulate _accrueDebtOnAction function (no-op in rev 4+).

        Revision 4+ removed _accrueDebtOnAction.

        Args:
            position: The user's debt position
            previous_scaled_balance: Balance before the action
            discount_percent: Ignored (no discount in rev 4+)
            index: Current variable debt index

        Returns:
            Always returns 0 (no discount)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # Calculate interest accrual without discount
        _ = wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=index,
        ) - wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=position.last_index or 0,
        )

        # Update last_index to match contract behavior
        position.last_index = index

        return 0

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,
        current_index: int,
        discount_percent: int,
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
