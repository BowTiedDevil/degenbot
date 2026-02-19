"""Collateral token processor for revision 4."""

import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.processors.base import BurnResult, CollateralBurnEvent, MathLibraries
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor


class CollateralV4Processor(CollateralV1Processor):
    """Processor for AToken revision 4."""

    revision = 4
    math_lib_version = "v3.4"

    def __init__(self) -> None:
        """Initialize with v3.4 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_4.wad_ray_math,
            percentage=aave_library_v3_4.percentage_math,
        )

    def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount from raw underlying amount.

        Uses floor division (ray_div_floor) to match revision 4 AToken
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

        Uses ceiling division (ray_div_ceil) to match revision 4 AToken
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
        Revision 4 uses ceiling division (ray_div_ceil) to match
        TokenMath.getATokenBurnScaledAmount behavior.

        For V4+, the scaled amount must be calculated from the original
        withdrawal amount in the WITHDRAW event, not from the Burn event's
        value + balance_increase. When scaled_delta is provided, it is
        used directly; otherwise, falls back to calculating from event data.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Pre-calculated scaled amount from original withdrawal amount.
                Must be provided for accurate processing in V4+.

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
