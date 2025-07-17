from degenbot.uniswap.v3_libraries import full_math, sqrt_price_math
from degenbot.uniswap.v3_types import SqrtPriceX96

type AmountIn = int
type AmountOut = int
type FeeTaken = int


def compute_swap_step(
    sqrt_ratio_x96_current: int,
    sqrt_ratio_x96_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[SqrtPriceX96, AmountIn, AmountOut, FeeTaken]:
    zero_for_one = sqrt_ratio_x96_current >= sqrt_ratio_x96_target
    exact_in = amount_remaining >= 0

    assert liquidity >= 0

    if exact_in:
        amount_remaining_minus_fee = full_math.muldiv(amount_remaining, 1000000 - fee_pips, 1000000)
        amount_in = (
            sqrt_price_math.get_amount0_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_target,
                sqrt_ratio_b_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                round_up=True,
            )
            if zero_for_one
            else sqrt_price_math.get_amount1_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_current,
                sqrt_ratio_b_x96=sqrt_ratio_x96_target,
                liquidity=liquidity,
                round_up=True,
            )
        )
        if amount_remaining_minus_fee >= amount_in:
            sqrt_ratio_x96_next = sqrt_ratio_x96_target
        else:
            sqrt_ratio_x96_next = sqrt_price_math.get_next_sqrt_price_from_input(
                sqrt_price_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                amount_in=amount_remaining_minus_fee,
                zero_for_one=zero_for_one,
            )
    else:
        amount_out = (
            sqrt_price_math.get_amount1_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_target,
                sqrt_ratio_b_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                round_up=False,
            )
            if zero_for_one
            else sqrt_price_math.get_amount0_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_current,
                sqrt_ratio_b_x96=sqrt_ratio_x96_target,
                liquidity=liquidity,
                round_up=False,
            )
        )
        if -amount_remaining >= amount_out:
            sqrt_ratio_x96_next = sqrt_ratio_x96_target
        else:
            sqrt_ratio_x96_next = sqrt_price_math.get_next_sqrt_price_from_output(
                sqrt_price_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                amount_out=-amount_remaining,
                zero_for_one=zero_for_one,
            )

    reached_target_price = sqrt_ratio_x96_target == sqrt_ratio_x96_next
    # get the input/output amounts
    if zero_for_one:
        amount_in = (
            amount_in
            if (reached_target_price and exact_in)
            else sqrt_price_math.get_amount0_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_next,
                sqrt_ratio_b_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                round_up=True,
            )
        )
        amount_out = (
            amount_out
            if (reached_target_price and not exact_in)
            else sqrt_price_math.get_amount1_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_next,
                sqrt_ratio_b_x96=sqrt_ratio_x96_current,
                liquidity=liquidity,
                round_up=False,
            )
        )
    else:
        amount_in = (
            amount_in
            if (reached_target_price and exact_in)
            else sqrt_price_math.get_amount1_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_current,
                sqrt_ratio_b_x96=sqrt_ratio_x96_next,
                liquidity=liquidity,
                round_up=True,
            )
        )
        amount_out = (
            amount_out
            if (reached_target_price and not exact_in)
            else sqrt_price_math.get_amount0_delta(
                sqrt_ratio_a_x96=sqrt_ratio_x96_current,
                sqrt_ratio_b_x96=sqrt_ratio_x96_next,
                liquidity=liquidity,
                round_up=False,
            )
        )

    # cap the output amount to not exceed the remaining output amount
    if not exact_in and (amount_out > -amount_remaining):
        amount_out = -amount_remaining

    if exact_in and (sqrt_ratio_x96_next != sqrt_ratio_x96_target):
        # we didn't reach the target, so take the remainder of the maximum input as fee
        fee_amount = amount_remaining - amount_in
    else:
        fee_amount = full_math.muldiv_rounding_up(amount_in, fee_pips, 1000000 - fee_pips)

    return sqrt_ratio_x96_next, amount_in, amount_out, fee_amount
