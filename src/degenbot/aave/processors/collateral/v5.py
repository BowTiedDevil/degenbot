"""Collateral token processor for revision 5."""

import degenbot.aave.libraries.v3_5 as aave_library_v3_5
from degenbot.aave.processors.base import (
    BurnResult,
    CollateralBurnEvent,
    CollateralMintEvent,
    MathLibraries,
    MintResult,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV5Processor(CollateralV1Processor):
    """Processor for AToken revision 5."""

    revision = 5
    math_lib_version = "v3.5"

    def __init__(self) -> None:
        """Initialize with v3.5 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_5.wad_ray_math,
            percentage=aave_library_v3_5.percentage_math,
        )

    def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount from raw underlying amount.

        Uses floor division (ray_div_floor) to match revision 5 AToken
        supply behavior. This version uses TokenMath.getATokenMintScaledAmount
        which rounds down.

        For withdraw operations, use calculate_burn_scaled_amount instead.

        Args:
            raw_amount: The raw underlying token amount
            index: The current liquidity index

        Returns:
            The scaled amount
        """
        return self._math_libs["wad_ray"].ray_div_floor(
            a=raw_amount,
            b=index,
        )

    def calculate_burn_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount for burn operations (WITHDRAW).

        Uses ceiling division (ray_div_ceil) to match revision 5 AToken
        withdraw behavior. This version uses TokenMath.getATokenBurnScaledAmount
        which rounds up.

        Args:
            raw_amount: The raw underlying token amount
            index: The current liquidity index

        Returns:
            The scaled amount
        """
        return self._math_libs["wad_ray"].ray_div_ceil(
            a=raw_amount,
            b=index,
        )

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> BurnResult:
        """
        Process a collateral burn event.

        Burn events are triggered by WITHDRAW operations.
        Revision 5 uses ceiling division (ray_div_ceil) to match
        TokenMath.getATokenBurnScaledAmount behavior.

        For V5+, the scaled amount must be calculated from the original
        withdrawal amount in the WITHDRAW event, not from the Burn event's
        value + balance_increase. When scaled_delta is provided, it is
        used directly; otherwise, falls back to calculating from event data.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Pre-calculated scaled amount from original withdrawal amount.
                Must be provided for accurate processing in V5+.

        Returns:
            BurnResult with balance_delta and new_index
        """
        if scaled_delta is not None:
            # Use pre-calculated scaled amount from withdrawal amount
            return BurnResult(
                balance_delta=-scaled_delta,
                new_index=event_data.index,
            )

        # Fallback: calculate from event data (may have rounding discrepancies)
        wad_ray_math = self._math_libs["wad_ray"]
        requested_amount = event_data.value + event_data.balance_increase
        balance_delta = -wad_ray_math.ray_div_ceil(
            a=requested_amount,
            b=event_data.index,
        )

        return BurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> MintResult:
        """
        Process a collateral mint event.

        Overrides parent to handle pure interest accrual with floor division
        for V5+ aTokens (uses TokenMath which rounds down for mints).

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
        elif event_data.value > event_data.balance_increase:
            # Standard deposit
            if scaled_delta is not None:
                balance_delta = scaled_delta
            else:
                balance_delta = wad_ray_math.ray_div(
                    a=event_data.value - event_data.balance_increase,
                    b=event_data.index,
                )
            is_repay = False
        else:
            # Pure interest accrual: value == balance_increase
            # This represents interest being added to total supply, but the user's
            # scaled balance doesn't change - only the index updates.
            # The balance_delta should be 0, not the scaled amount.
            balance_delta = 0
            is_repay = False

        return MintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=is_repay,
        )
