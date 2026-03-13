"""Collateral token processor for revision 4."""

import degenbot.aave.libraries
from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    MathLibraries,
    ScaledTokenBurnResult,
    ScaledTokenMintResult,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV4Processor(CollateralV1Processor):
    """Processor for AToken revision 4."""

    revision = 4

    def __init__(self) -> None:
        self._math_libs = MathLibraries(
            wad_ray=degenbot.aave.libraries.wad_ray_math,
            percentage=degenbot.aave.libraries.percentage_math,
        )

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> ScaledTokenBurnResult:
        """
        Process a collateral burn event.

        Burn events are triggered by WITHDRAW operations.
        Revision 4 uses ceiling division (ray_div_ceil) to match
        TokenMath.getATokenBurnScaledAmount behavior.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation

        Returns:
            BurnResult with balance_delta and new_index
        """
        if event_data.scaled_amount is not None:
            # Use pre-calculated scaled amount from Pool contract
            return ScaledTokenBurnResult(
                balance_delta=-event_data.scaled_amount,
                new_index=event_data.index,
            )

        wad_ray_math = self._math_libs["wad_ray"]

        # uint256 amountToBurn = amount + balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDivCeil(index);
        balance_delta = -wad_ray_math.ray_div_ceil(
            a=requested_amount,
            b=event_data.index,
        )

        return ScaledTokenBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> ScaledTokenMintResult:
        """
        Process a collateral mint event.

        Overrides parent to handle pure interest accrual with floor division
        for V4+ aTokens (uses TokenMath which rounds down for mints).

        Mint events can be triggered by:
        - SUPPLY: value > balance_increase
        - WITHDRAW: balance_increase > value (interest accrual)
        - Interest accrual: value == balance_increase
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.balance_increase > event_data.value:
            # Interest accrual exceeds deposit amount - emitted during withdraw
            requested_amount = event_data.balance_increase - event_data.value
            balance_delta = -wad_ray_math.ray_div(
                a=requested_amount,
                b=event_data.index,
            )
            is_repay = True
        elif event_data.value >= event_data.balance_increase:
            # Standard deposit
            if event_data.scaled_amount is not None:
                # Use pre-calculated scaled amount from Pool contract
                balance_delta = event_data.scaled_amount
            else:
                balance_delta = wad_ray_math.ray_div(
                    a=event_data.value - event_data.balance_increase,
                    b=event_data.index,
                )
            is_repay = False
        else:
            # value == balance_increase: deposit amount equals accrued interest, OR
            # pure interest accrual without a deposit (e.g., before transfer).
            # If scaled_amount is provided from a matched SUPPLY event, it's a deposit.
            # Otherwise, it's pure interest accrual where only the index updates.
            balance_delta = event_data.scaled_amount if event_data.scaled_amount is not None else 0
            is_repay = False

        return ScaledTokenMintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=is_repay,
        )
