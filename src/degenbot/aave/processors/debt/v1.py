"""Debt token processor for revision 1."""

import degenbot.aave.libraries as aave_library
from degenbot.aave.processors.base import (
    BurnResult,
    DebtBurnEvent,
    DebtMintEvent,
    DebtTokenProcessor,
    MathLibraries,
    MintResult,
)


class DebtV1Processor(DebtTokenProcessor):
    """Processor for VToken revision 1."""

    revision = 1
    math_lib_version = "v3.1"

    def __init__(self) -> None:
        """Initialize with math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library.wad_ray_math,
            percentage=aave_library.percentage_math,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision."""
        return self._math_libs

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> MintResult:
        """
        Process a debt mint event.

        Mint events can be triggered by:
        - BORROW: value > balance_increase
        - REPAY: balance_increase > value (interest accrual)

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Unused for revisions 1-3 (calculated from event data)

        Returns:
            MintResult with balance_delta, new_index, and is_repay flag
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

        return MintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=is_repay,
        )

    def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount from raw underlying amount.

        Uses standard ray_div (half up rounding) to match revision 1-3 vToken
        behavior.

        Args:
            raw_amount: The raw underlying token amount
            index: The current borrow index

        Returns:
            The scaled amount
        """
        return self._math_libs["wad_ray"].ray_div(
            a=raw_amount,
            b=index,
        )

    def calculate_mint_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount for mint operations.

        For V1, uses the same calculation as burn (standard ray_div).

        Args:
            raw_amount: The raw underlying token amount
            index: The current borrow index

        Returns:
            The scaled amount
        """
        return self.calculate_scaled_amount(raw_amount=raw_amount, index=index)

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> BurnResult:
        """
        Process a debt burn event.

        Burn events are triggered by REPAY operations.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Unused for revisions 1-3 (calculated from event data)

        Returns:
            BurnResult with balance_delta and new_index
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        balance_delta = -wad_ray_math.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        return BurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )
