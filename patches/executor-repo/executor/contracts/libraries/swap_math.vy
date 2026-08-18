"""
Port of UniswapV3 SwapMath.sol.

Source: contracts/libraries/SwapMath.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/SwapMath.sol

Computes the result of a swap within a single tick price range.
This is the top-level swap step function that the pool's swap()
method calls, orchestrating SqrtPriceMath for price/amount deltas.


"""

from . import full_math
from . import sqrt_price_math


# ═══════════════════════════════════════════════════════════════════════════
# computeSwapStep — the real V3 swap step computation
# ═══════════════════════════════════════════════════════════════════════════


@internal
@pure
def compute_swap_step(
    sqrt_ratio_current_x96: uint160,
    sqrt_ratio_target_x96: uint160,
    liquidity: uint128,
    amount_remaining: int256,
    fee_pips: uint24,
) -> (uint160, uint256, uint256, uint256):
    """Computes the result of swapping some amount in, or amount out,
    given the parameters of the swap.

    Parameters:
        sqrt_ratio_current_x96: The current sqrt price of the pool
        sqrt_ratio_target_x96:  The price that cannot be exceeded,
                                 from which the direction of the swap is inferred
        liquidity:              The usable liquidity
        amount_remaining:       How much input or output amount is remaining
                                 to be swapped in/out (positive=exactIn, negative=exactOut)
        fee_pips:               The fee taken from the input amount, expressed
                                 in hundredths of a bip (e.g., 3000 for 0.3%)

    Returns:
        sqrt_ratio_next_x96: The price after swapping the amount in/out,
                               not to exceed the price target
        amount_in:            The amount to be swapped in
        amount_out:           The amount to be received
        fee_amount:           The amount of input that will be taken as a fee
    """
    zero_for_one: bool = sqrt_ratio_current_x96 >= sqrt_ratio_target_x96
    exact_in: bool = amount_remaining >= 0

    # Compute absolute value of amount_remaining as uint256.
    # Vyper's convert(int256 → uint256) reverts for negative values,
    # and abs() also reverts for MIN_INT256. We use unsafe_sub to negate,
    # then check if the result is still negative (which only happens for
    # MIN_INT256 where negation wraps back to itself).
    abs_amount_remaining: uint256 = empty(uint256)
    if exact_in:
        abs_amount_remaining = convert(amount_remaining, uint256)
    else:
        negated: int256 = unsafe_sub(0, amount_remaining)
        if negated >= 0:
            abs_amount_remaining = convert(negated, uint256)
        else:
            # amount_remaining == MIN_INT256: |x| = 2^255
            abs_amount_remaining = convert(1, uint256) << convert(255, uint256)

    sqrt_ratio_next_x96: uint160 = empty(uint160)
    amount_in: uint256 = empty(uint256)
    amount_out: uint256 = empty(uint256)

    if exact_in:
        amount_remaining_less_fee: uint256 = full_math.mul_div(
            abs_amount_remaining, 1000000 - convert(fee_pips, uint256), 1000000
        )
        # Compute max amount in to reach the target price
        if zero_for_one:
            amount_in = sqrt_price_math.get_amount0_delta(
                sqrt_ratio_target_x96, sqrt_ratio_current_x96, liquidity, True
            )
        else:
            amount_in = sqrt_price_math.get_amount1_delta(
                sqrt_ratio_current_x96, sqrt_ratio_target_x96, liquidity, True
            )
        if amount_remaining_less_fee >= amount_in:
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96
        else:
            sqrt_ratio_next_x96 = sqrt_price_math.get_next_sqrt_price_from_input(
                sqrt_ratio_current_x96, liquidity, amount_remaining_less_fee, zero_for_one
            )
    else:
        # Compute max amount out to reach the target price
        if zero_for_one:
            amount_out = sqrt_price_math.get_amount1_delta(
                sqrt_ratio_target_x96, sqrt_ratio_current_x96, liquidity, False
            )
        else:
            amount_out = sqrt_price_math.get_amount0_delta(
                sqrt_ratio_current_x96, sqrt_ratio_target_x96, liquidity, False
            )
        if abs_amount_remaining >= amount_out:
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96
        else:
            sqrt_ratio_next_x96 = sqrt_price_math.get_next_sqrt_price_from_output(
                sqrt_ratio_current_x96, liquidity, abs_amount_remaining, zero_for_one
            )

    # reached_target: whether we reached the target price
    reached_target: bool = sqrt_ratio_target_x96 == sqrt_ratio_next_x96

    # Get the input/output amounts based on the next price
    if zero_for_one:
        if reached_target and exact_in:
            pass  # amount_in already set
        else:
            amount_in = sqrt_price_math.get_amount0_delta(
                sqrt_ratio_next_x96, sqrt_ratio_current_x96, liquidity, True
            )
        if reached_target and not exact_in:
            pass  # amount_out already set
        else:
            amount_out = sqrt_price_math.get_amount1_delta(
                sqrt_ratio_next_x96, sqrt_ratio_current_x96, liquidity, False
            )
    else:
        if reached_target and exact_in:
            pass  # amount_in already set
        else:
            amount_in = sqrt_price_math.get_amount1_delta(
                sqrt_ratio_current_x96, sqrt_ratio_next_x96, liquidity, True
            )
        if reached_target and not exact_in:
            pass  # amount_out already set
        else:
            amount_out = sqrt_price_math.get_amount0_delta(
                sqrt_ratio_current_x96, sqrt_ratio_next_x96, liquidity, False
            )

    # Cap the output amount to not exceed the remaining output amount
    if not exact_in and amount_out > abs_amount_remaining:
        amount_out = abs_amount_remaining

    # Compute fee
    fee_amount: uint256 = empty(uint256)
    if exact_in and not reached_target:
        fee_amount = abs_amount_remaining - amount_in
    else:
        fee_amount = full_math.mul_div_rounding_up(
            amount_in, convert(fee_pips, uint256), 1000000 - convert(fee_pips, uint256)
        )

    return sqrt_ratio_next_x96, amount_in, amount_out, fee_amount
