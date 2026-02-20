"""Debt token processor for revision 5."""

import degenbot.aave.libraries as aave_library_v3_5
from degenbot.aave.processors.base import (
    BurnResult,
    DebtBurnEvent,
    DebtMintEvent,
    MathLibraries,
    MintResult,
)
from degenbot.aave.processors.debt.v1 import DebtV1Processor


class DebtV5Processor(DebtV1Processor):
    """Processor for VToken revision 5."""

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

        Uses floor division (ray_div_floor) to match revision 5 vToken
        burn behavior. This version uses TokenMath.getVTokenBurnScaledAmount
        which rounds down.

        For mint operations (BORROW), use ray_div_ceil instead.

        Args:
            raw_amount: The raw underlying token amount
            index: The current borrow index

        Returns:
            The scaled amount
        """
        return self._math_libs["wad_ray"].ray_div_floor(
            a=raw_amount,
            b=index,
        )

    def calculate_mint_scaled_amount(self, raw_amount: int, index: int) -> int:
        """
        Calculate scaled amount for mint operations.

        Uses ceiling division (ray_div_ceil) to match revision 5 vToken
        mint behavior. This version uses TokenMath.getVTokenMintScaledAmount
        which rounds up.

        Args:
            raw_amount: The raw underlying token amount
            index: The current borrow index

        Returns:
            The scaled amount
        """
        return self._math_libs["wad_ray"].ray_div_ceil(
            a=raw_amount,
            b=index,
        )

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> BurnResult:
        """
        Process a debt burn event.

        Burn events are triggered by REPAY operations.
        Revision 5 (v3.5) uses floor division (ray_div_floor) to match
        TokenMath.getVTokenBurnScaledAmount behavior.

        For V5+, the scaled amount must be calculated from the original
        paybackAmount in the REPAY event, not from the Burn event's
        value + balance_increase. When scaled_delta is provided, it is
        used directly; otherwise, falls back to calculating from event data.

        Args:
            event_data: The burn event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Pre-calculated scaled amount from original paybackAmount.
                Must be provided for accurate processing in V5+.

        Returns:
            BurnResult with balance_delta and new_index
        """
        if scaled_delta is not None:
            # Use pre-calculated scaled amount from paybackAmount
            return BurnResult(
                balance_delta=-scaled_delta,
                new_index=event_data.index,
            )

        # Fallback: calculate from event data (may have rounding discrepancies)
        wad_ray_math = self._math_libs["wad_ray"]
        requested_amount = event_data.value + event_data.balance_increase
        balance_delta = -wad_ray_math.ray_div_floor(
            a=requested_amount,
            b=event_data.index,
        )

        return BurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> MintResult:
        """
        Process a debt mint event.

        Mint events can be triggered by:
        - BORROW: value > balance_increase (new debt)
        - REPAY: balance_increase > value (interest accrual before repayment)

        Revision 5 uses ceiling division (ray_div_ceil) for BORROW operations
        to match TokenMath.getVTokenMintScaledAmount behavior.

        For V5+, the scaled amount for BORROW operations must be calculated
        from the original borrow amount in the BORROW event, not from the
        Mint event's value - balance_increase. When scaled_delta is provided
        for BORROW operations, it is used directly.

        Args:
            event_data: The mint event data
            previous_balance: The user's balance before this event
            previous_index: The index at previous_balance calculation
            scaled_delta: Pre-calculated scaled amount from original borrow amount.
                Must be provided for accurate BORROW processing in V5+.

        Returns:
            MintResult with balance_delta, new_index, and is_repay flag
        """
        wad_ray_math = self._math_libs["wad_ray"]

        if event_data.value > event_data.balance_increase:
            # BORROW path: emitted in _mintScaled
            if scaled_delta is not None:
                # Use pre-calculated scaled amount from borrow amount
                return MintResult(
                    balance_delta=scaled_delta,
                    new_index=event_data.index,
                    is_repay=False,
                )

            # Fallback: calculate from event data (may have rounding discrepancies)
            # Solidity: uint256 amountToMint = amount + balanceIncrease;
            requested_amount = event_data.value - event_data.balance_increase
            balance_delta = wad_ray_math.ray_div_ceil(
                a=requested_amount,
                b=event_data.index,
            )
            return MintResult(
                balance_delta=balance_delta,
                new_index=event_data.index,
                is_repay=False,
            )

        # REPAY path: emitted in _burnScaled (interest accrual)
        # Solidity: uint256 amountToMint = balanceIncrease - amount;
        requested_amount = event_data.balance_increase - event_data.value
        balance_delta = -wad_ray_math.ray_div_floor(
            a=requested_amount,
            b=event_data.index,
        )
        return MintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=True,
        )
