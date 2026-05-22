"""Unified token processor implementation."""

from typing import ClassVar

import degenbot.aave.libraries.percentage_math
import degenbot.aave.libraries.wad_ray_math
from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    GhoScaledTokenBurnResult,
    GhoScaledTokenMintResult,
    GhoUserOperation,
    MathLibraries,
    ScaledTokenBurnResult,
    ScaledTokenMintResult,
)
from degenbot.aave.processors.strategies import (
    DiscountStrategy,
    RoundingMode,
    RoundingStrategy,
)


class UnifiedCollateralProcessor:
    """Unified processor for collateral (aToken) operations.

    Implements CollateralTokenProcessor protocol with strategy-based rounding.
    """

    revision: ClassVar[int] = 0  # Set by factory based on actual revision

    def __init__(
        self,
        rounding: RoundingStrategy,
        revision: int,
    ) -> None:
        """Initialize the instance."""
        self._rounding = rounding
        self._revision = revision
        self._math_libs = MathLibraries(
            _ray_div=degenbot.aave.libraries.wad_ray_math.ray_div,
            _ray_div_ceil=degenbot.aave.libraries.wad_ray_math.ray_div_ceil,
            _ray_div_floor=degenbot.aave.libraries.wad_ray_math.ray_div_floor,
            _ray_mul=degenbot.aave.libraries.wad_ray_math.ray_mul,
            _percent_div=degenbot.aave.libraries.percentage_math.percent_div,
            _percent_mul=degenbot.aave.libraries.percentage_math.percent_mul,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision.

        Returns:
            The computed value.

        """
        return self._math_libs

    def _ray_div(self, a: int, b: int, mode: RoundingMode) -> int:
        """Perform ray division with specified rounding mode.

        Returns:
            The computed value.

        """
        match mode:
            case RoundingMode.HALF_UP:
                return self._math_libs.ray_div(a, b)
            case RoundingMode.FLOOR:
                return self._math_libs.ray_div_floor(a, b)
            case RoundingMode.CEIL:
                return self._math_libs.ray_div_ceil(a, b)

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> ScaledTokenMintResult:
        """Process a collateral mint event.

        Mint events can be triggered by:
        - SUPPLY: value > balance_increase
        - WITHDRAW: balance_increase > value (interest accrual)
        - Interest accrual: value == balance_increase

        Returns:
            The computed value.

        """
        if event_data.balance_increase > event_data.value:
            # Interest accrual exceeds deposit amount - emitted during withdraw
            # This occurs when withdrawing and interest accrual is minted first
            requested_amount = event_data.balance_increase - event_data.value
            # Withdrawal interest path uses HALF_UP (same for all revisions)
            balance_delta = -self._ray_div(
                a=requested_amount,
                b=event_data.index,
                mode=RoundingMode.HALF_UP,
            )
            is_repay = True
        elif event_data.value >= event_data.balance_increase:
            # Standard deposit - emitted in _mintScaled during supply
            if event_data.scaled_amount is not None:
                # Use pre-calculated scaled amount from Pool contract
                balance_delta = event_data.scaled_amount
            else:
                requested_amount = event_data.value - event_data.balance_increase
                balance_delta = self._ray_div(
                    a=requested_amount,
                    b=event_data.index,
                    mode=self._rounding.mint_rounding,
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

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> ScaledTokenBurnResult:
        """Process a collateral burn event.

        Burn events are triggered by WITHDRAW operations.

        Returns:
            The computed value.

        """
        if scaled_delta is not None:
            # Use pre-calculated scaled amount from withdraw amount
            # This is critical for WITHDRAW with Mint event when interest > withdrawal
            return ScaledTokenBurnResult(
                balance_delta=-scaled_delta,
                new_index=event_data.index,
            )

        if event_data.scaled_amount is not None:
            # Use pre-calculated scaled amount from Pool contract
            return ScaledTokenBurnResult(
                balance_delta=-event_data.scaled_amount,
                new_index=event_data.index,
            )

        # Fallback: calculate from event data (for standard Burn events)
        # uint256 amountToBurn = amount + balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        balance_delta = -self._ray_div(
            a=requested_amount,
            b=event_data.index,
            mode=self._rounding.burn_rounding,
        )

        return ScaledTokenBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )


class UnifiedDebtProcessor:
    """Unified processor for debt (vToken) operations.

    Implements DebtTokenProcessor protocol with strategy-based rounding.
    """

    revision: ClassVar[int] = 0  # Set by factory based on actual revision

    def __init__(
        self,
        rounding: RoundingStrategy,
        revision: int,
    ) -> None:
        """Initialize the instance."""
        self._rounding = rounding
        self._revision = revision
        self._math_libs = MathLibraries(
            _ray_div=degenbot.aave.libraries.wad_ray_math.ray_div,
            _ray_div_ceil=degenbot.aave.libraries.wad_ray_math.ray_div_ceil,
            _ray_div_floor=degenbot.aave.libraries.wad_ray_math.ray_div_floor,
            _ray_mul=degenbot.aave.libraries.wad_ray_math.ray_mul,
            _percent_div=degenbot.aave.libraries.percentage_math.percent_div,
            _percent_mul=degenbot.aave.libraries.percentage_math.percent_mul,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision.

        Returns:
            The computed value.

        """
        return self._math_libs

    def _ray_div(self, a: int, b: int, mode: RoundingMode) -> int:
        """Perform ray division with specified rounding mode.

        Returns:
            The computed value.

        """
        match mode:
            case RoundingMode.HALF_UP:
                return self._math_libs.ray_div(a, b)
            case RoundingMode.FLOOR:
                return self._math_libs.ray_div_floor(a, b)
            case RoundingMode.CEIL:
                return self._math_libs.ray_div_ceil(a, b)

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,  # noqa: ARG002
    ) -> ScaledTokenMintResult:
        """Process a debt mint event.

        Mint events can be triggered by:
        - BORROW: value > balance_increase
        - REPAY: balance_increase > value (interest accrual)

        Returns:
            The computed value.

        """
        if event_data.value >= event_data.balance_increase:
            # BORROW path: emitted in _mintScaled
            if event_data.scaled_amount is not None:
                # Use pre-calculated scaled amount from BORROW event
                balance_delta = event_data.scaled_amount
            else:
                # Fallback: calculate from event data
                # Solidity: uint256 amountToMint = amount + balanceIncrease;
                requested_amount = event_data.value - event_data.balance_increase
                balance_delta = self._ray_div(
                    a=requested_amount,
                    b=event_data.index,
                    mode=self._rounding.mint_rounding,
                )
            is_repay = False
        else:
            # REPAY path: emitted in _burnScaled
            # The Mint event is emitted when interest > repayment amount.
            # The net balance change is just burning the scaled repayment amount.
            # amount = balanceIncrease - value (from Mint event)
            amount_repaid = event_data.balance_increase - event_data.value
            balance_delta = -self._ray_div(
                a=amount_repaid,
                b=event_data.index,
                mode=self._rounding.burn_rounding,
            )
            is_repay = True

        return ScaledTokenMintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            is_repay=is_repay,
        )

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,  # noqa: ARG002
        previous_index: int,  # noqa: ARG002
        scaled_delta: int | None = None,
    ) -> ScaledTokenBurnResult:
        """Process a debt burn event.

        Burn events are triggered by REPAY operations.

        Returns:
            The computed value.

        """
        if scaled_delta is not None:
            # Use pre-calculated scaled amount from paybackAmount
            # This is critical for REPAY with Mint event when interest > repayment
            return ScaledTokenBurnResult(
                balance_delta=-scaled_delta,
                new_index=event_data.index,
            )

        # Fallback: calculate from event data (for standard Burn events)
        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        balance_delta = -self._ray_div(
            a=requested_amount,
            b=event_data.index,
            mode=self._rounding.burn_rounding,
        )

        return ScaledTokenBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
        )


class UnifiedGhoProcessor:
    """Unified processor for GHO debt operations.

    Implements GhoDebtTokenProcessor protocol with strategy-based rounding
    and optional discount support.
    """

    revision: ClassVar[int] = 0  # Set by factory based on actual revision

    def __init__(
        self,
        rounding: RoundingStrategy,
        discount: DiscountStrategy,
        revision: int,
    ) -> None:
        """Initialize the instance."""
        self._rounding = rounding
        self._discount = discount
        self._revision = revision
        self._math_libs = MathLibraries(
            _ray_div=degenbot.aave.libraries.wad_ray_math.ray_div,
            _ray_div_ceil=degenbot.aave.libraries.wad_ray_math.ray_div_ceil,
            _ray_div_floor=degenbot.aave.libraries.wad_ray_math.ray_div_floor,
            _ray_mul=degenbot.aave.libraries.wad_ray_math.ray_mul,
            _percent_div=degenbot.aave.libraries.percentage_math.percent_div,
            _percent_mul=degenbot.aave.libraries.percentage_math.percent_mul,
        )

    def get_math_libraries(self) -> MathLibraries:
        """Get the math libraries for this revision.

        Returns:
            The computed value.

        """
        return self._math_libs

    def _ray_div(self, a: int, b: int, mode: RoundingMode) -> int:
        """Perform ray division with specified rounding mode.

        Returns:
            The computed value.

        """
        match mode:
            case RoundingMode.HALF_UP:
                return self._math_libs.ray_div(a, b)
            case RoundingMode.FLOOR:
                return self._math_libs.ray_div_floor(a, b)
            case RoundingMode.CEIL:
                return self._math_libs.ray_div_ceil(a, b)

    def supports_discount(self) -> bool:
        """Check if this revision supports the discount mechanism.

        Returns:
            The computed value.

        """
        return self._discount.supports_discount

    def process_mint_event(
        self,
        event_data: DebtMintEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
        actual_repay_amount: int | None = None,
    ) -> GhoScaledTokenMintResult:
        """Process a GHO debt mint event.

        Returns:
            The computed value.

        """
        # Accrue debt with discount (stateless - doesn't mutate position)
        discount_scaled = self.accrue_debt_on_action(
            previous_scaled_balance=previous_balance,
            previous_index=previous_index,
            discount_percent=previous_discount,
            current_index=event_data.index,
        )

        if event_data.value >= event_data.balance_increase:
            # GHO BORROW: emitted in _mintScaled
            #
            # For V5+ (no discount, CEIL/FLOOR rounding), prefer the
            # pre-calculated scaled_amount from the enrichment layer.
            # It is computed via TokenMath which matches the on-chain rayDiv
            # exactly for V5+. Deriving from Mint event fields introduces 1-wei
            # rounding errors because `balanceIncrease` is itself rounded
            # from rayMul, making `value - balanceIncrease` an approximation
            # of the raw borrow amount.
            #
            # For V4 and V1-V3, do NOT use scaled_amount because the
            # enrichment layer calculates it using pool-level TokenMath
            # (CEIL/FLOOR for V4+ pools) which differs from GHO V4's
            # HALF_UP rounding. For those revisions, derive from event data
            # using the processor's own rounding mode.
            if (
                not self._discount.supports_discount
                and self._rounding.mint_rounding == RoundingMode.CEIL
                and event_data.scaled_amount is not None
            ):
                balance_delta = event_data.scaled_amount
            else:
                requested_amount = event_data.value - event_data.balance_increase
                balance_delta = self._ray_div(
                    a=requested_amount,
                    b=event_data.index,
                    mode=self._rounding.mint_rounding,
                )

            if self._discount.supports_discount:
                if balance_delta > discount_scaled:
                    balance_delta -= discount_scaled
                else:
                    balance_delta = -(discount_scaled - balance_delta)

            user_operation = GhoUserOperation.GHO_BORROW

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY: emitted in _burnScaled
            # The Mint event is emitted when interest > repayment amount.
            # Use actual_repay_amount from Repay event if provided to avoid
            # 1 wei rounding errors from deriving from Mint event fields.
            if actual_repay_amount is not None:
                amount_repaid = actual_repay_amount
            else:
                # Fallback: derive from Mint event (may have 1 wei rounding error)
                amount_repaid = event_data.balance_increase - event_data.value

            repayment_scaled = self._ray_div(
                a=amount_repaid,
                b=event_data.index,
                mode=self._rounding.burn_rounding,
            )

            if self._discount.supports_discount and self._discount.has_discounted_balance_method:
                # Get balance before burn with discount
                balance_before_burn = self.get_discounted_balance(
                    scaled_balance=previous_balance,
                    previous_index=previous_index,
                    current_index=event_data.index,
                    discount_percent=previous_discount,
                )

                if amount_repaid == balance_before_burn:
                    # Full repayment: burn all scaled balance
                    balance_delta = -previous_balance
                else:
                    # Partial repayment: burn (repayment_scaled + discount_scaled)
                    balance_delta = -(repayment_scaled + discount_scaled)
            elif self._discount.supports_discount:
                # V1 discount: no getDiscountedBalance method
                balance_delta = -(repayment_scaled + discount_scaled)
            else:
                # No discount (V4+)
                balance_delta = -repayment_scaled

            user_operation = GhoUserOperation.GHO_REPAY

        else:
            # Pure interest accrual (value == balance_increase)
            if self._discount.supports_discount:
                # Emitted from _accrueDebtOnAction during discount updates
                # The balance decreases by the discount amount (burned by contract)
                balance_delta = -discount_scaled
            else:
                # No discount (V4+): just interest accrual
                balance_increase = self._math_libs.ray_mul(
                    a=previous_balance,
                    b=event_data.index,
                ) - self._math_libs.ray_mul(
                    a=previous_balance,
                    b=previous_index,
                )

                # Convert back to scaled
                balance_increase_scaled = self._math_libs.ray_div(
                    a=balance_increase,
                    b=event_data.index,
                )

                balance_delta = balance_increase_scaled

            user_operation = GhoUserOperation.GHO_INTEREST_ACCRUAL

        # For GHO with discount, always refresh after balance-changing operations
        should_refresh_discount = self._discount.supports_discount

        return GhoScaledTokenMintResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            user_operation=user_operation,
            discount_scaled=discount_scaled,
            should_refresh_discount=should_refresh_discount,
        )

    def process_burn_event(
        self,
        event_data: DebtBurnEvent,
        previous_balance: int,
        previous_index: int,
        previous_discount: int,
    ) -> GhoScaledTokenBurnResult:
        """Process a GHO debt burn event.

        Returns:
            The computed value.

        """
        # For all GHO revisions, always calculate from event data.
        # The scaled_amount field is not used for GHO because GHO has its own
        # calculation logic that differs from standard debt tokens.
        #
        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # Calculate amount_scaled from either the pre-calculated enrichment
        # value (V5+, no discount, FLOOR burn rounding) or by deriving from
        # event fields.
        #
        # For V5+ (no discount, FLOOR burn rounding), prefer the pre-calculated
        # scaled_amount from the enrichment layer to avoid 1-wei rounding errors.
        # The enrichment uses TokenMath which matches V5+ on-chain calculations.
        #
        # For V4 and V1-V3, do NOT use scaled_amount because the enrichment
        # layer uses pool-level TokenMath (which differs from V4 GHO's HALF_UP).
        if (
            not self._discount.supports_discount
            and self._rounding.burn_rounding == RoundingMode.FLOOR
            and event_data.scaled_amount is not None
        ):
            amount_scaled = event_data.scaled_amount
        else:
            # uint256 amountScaled = amount.rayDiv(index);
            amount_scaled = self._ray_div(
                a=requested_amount,
                b=event_data.index,
                mode=self._rounding.burn_rounding,
            )

        # Accrue debt with discount (stateless - doesn't mutate position)
        discount_scaled = self.accrue_debt_on_action(
            previous_scaled_balance=previous_balance,
            previous_index=previous_index,
            discount_percent=previous_discount,
            current_index=event_data.index,
        )

        if self._discount.supports_discount and self._discount.has_discounted_balance_method:
            # Get balance before burn with discount
            balance_before_burn = self.get_discounted_balance(
                scaled_balance=previous_balance,
                previous_index=previous_index,
                current_index=event_data.index,
                discount_percent=previous_discount,
            )

            if requested_amount == balance_before_burn:
                # Full repayment: burn all scaled balance
                balance_delta = -previous_balance
            else:
                # Partial repayment
                balance_delta = -(amount_scaled + discount_scaled)
        elif self._discount.supports_discount:
            # V1 discount: no getDiscountedBalance method
            # Matches Solidity: _burn(user, (amountScaled + discountScaled).toUint128())
            balance_delta = -(amount_scaled + discount_scaled)
        else:
            # No discount (V4+): check for full repayment
            balance_before_burn = self._math_libs.ray_mul(
                a=previous_balance,
                b=event_data.index,
            )

            if requested_amount == balance_before_burn:
                # Full repayment: burn all scaled balance
                balance_delta = -previous_balance
            else:
                # Partial repayment
                balance_delta = -amount_scaled

        # For GHO with discount, always refresh after balance-changing operations
        should_refresh_discount = self._discount.supports_discount

        return GhoScaledTokenBurnResult(
            balance_delta=balance_delta,
            new_index=event_data.index,
            discount_scaled=discount_scaled,
            should_refresh_discount=should_refresh_discount,
        )

    def accrue_debt_on_action(
        self,
        previous_scaled_balance: int,
        previous_index: int,
        discount_percent: int,
        current_index: int,
    ) -> int:
        """Simulate _accrueDebtOnAction function (stateless version).

        Returns:
            The computed value.

        """
        if not self._discount.supports_discount:
            return 0

        # Calculate balance increase
        balance_increase = self._math_libs.ray_mul(
            a=previous_scaled_balance,
            b=current_index,
        ) - self._math_libs.ray_mul(
            a=previous_scaled_balance,
            b=previous_index,
        )

        discount_scaled = 0
        if balance_increase != 0 and discount_percent != 0:
            discount = self._math_libs.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )
            discount_scaled = self._math_libs.ray_div(a=discount, b=current_index)

        return discount_scaled

    def get_discounted_balance(
        self,
        scaled_balance: int,
        previous_index: int,
        current_index: int,
        discount_percent: int,
    ) -> int:
        """Calculate discounted balance for burn operations.

        Returns:
            The computed value.

        """
        if scaled_balance == 0:
            return 0

        balance = self._math_libs.ray_mul(
            a=scaled_balance,
            b=current_index,
        )

        if not self._discount.supports_discount:
            return balance

        if current_index == previous_index:
            return balance

        if discount_percent != 0:
            balance_increase = balance - self._math_libs.ray_mul(
                a=scaled_balance,
                b=previous_index,
            )

            balance -= self._math_libs.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )

        return balance


__all__ = [
    "UnifiedCollateralProcessor",
    "UnifiedDebtProcessor",
    "UnifiedGhoProcessor",
]
