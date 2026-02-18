"""Debt token processor for revision 1."""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_1 as aave_library_v3_1
from degenbot.aave.processors.base import (
    DebtBurnEvent,
    DebtMintEvent,
    DebtTokenProcessor,
    MathLibraries,
)

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3DebtPositionsTable


class DebtV1Processor(DebtTokenProcessor):
    """Processor for VToken revision 1."""

    revision = 1

    def __init__(self) -> None:
        """Initialize with math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_1.wad_ray_math,
            percentage=aave_library_v3_1.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        position: "AaveV3DebtPositionsTable",
        *,
        previous_discount: int = 0,  # noqa: ARG002
    ) -> tuple[int, bool]:
        """
        Process a debt mint event.

        Mint events can be triggered by:
        - BORROW: value > balance_increase
        - REPAY: balance_increase > value (interest accrual)

        Args:
            event_data: The mint event data
            position: The user's debt position to update
            previous_discount: Unused (for GHO compatibility)

        Returns:
            Tuple of (balance_delta, is_repay)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.value > event_data.balance_increase:
            # BORROW path: emitted in _mintScaled
            # Solidity: uint256 amountToMint = amount + balanceIncrease;
            requested_amount = event_data.value - event_data.balance_increase
            balance_delta = wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = False
        else:
            # REPAY path: emitted in _burnScaled
            # Solidity: uint256 amountToMint = balanceIncrease - amount;
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = True

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta, is_repay

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        position: "AaveV3DebtPositionsTable",
        *,
        previous_discount: int = 0,  # noqa: ARG002
    ) -> int:
        """
        Process a debt burn event.

        Burn events are triggered by REPAY operations.

        Args:
            event_data: The burn event data
            previous_discount: Unused (for GHO compatibility)
            position: The user's debt position to update

        Returns:
            The balance delta (negative for repayment)
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        balance_delta = -wad_ray_math.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        position.balance += balance_delta
        position.last_index = event_data.index

        return balance_delta
