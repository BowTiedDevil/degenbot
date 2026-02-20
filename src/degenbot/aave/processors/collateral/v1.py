"""Collateral token processor for revision 1."""

import degenbot.aave.libraries as aave_library
from degenbot.aave.processors.base import (
    BurnResult,
    CollateralBurnEvent,
    CollateralMintEvent,
    CollateralTokenProcessor,
    MathLibraries,
    MintResult,
)


class CollateralV1Processor(CollateralTokenProcessor):
    """Processor for AToken revision 1."""

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
        event_data: CollateralMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
    ) -> MintResult:
        """
        Process a collateral mint event.

        Mint events can be triggered by:
        - SUPPLY: value > balance_increase
        - WITHDRAW: balance_increase > value (interest accrual)
        - Interest accrual: value == balance_increase

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            MintResult with balance_delta, new_index, and is_repay flag
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.balance_increase > event_data.value:
            # Interest accrual exceeds deposit amount - emitted during withdraw
            # This occurs when withdrawing and interest accrual is minted first
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = True
        elif event_data.value > event_data.balance_increase:
            # Standard deposit - emitted in _mintScaled during supply
            if event_data.scaled_amount is not None:
                # Use pre-calculated scaled amount from Pool contract
                balance_delta = event_data.scaled_amount
            else:
                requested_amount = event_data.value - event_data.balance_increase
                balance_delta = wad_ray_math.ray_div(
                    a=requested_amount,
                    b=event_data.index,
                )
            is_repay = False
        else:
            # Pure interest accrual: value == balance_increase
            # Emitted when interest accrues without a deposit (e.g., before transfer).
            # The user's scaled balance doesn't change - only the index updates.
            # The interest tokens are minted via ERC20 Transfer, not via this Mint event.
            balance_delta = 0
            is_repay = False

        return MintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=is_repay,
        )

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
    ) -> BurnResult:
        """
        Process a collateral burn event.

        Burn events are triggered by WITHDRAW operations.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            BurnResult with balance_delta and new_index
        """
        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount + balanceIncrease;
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
