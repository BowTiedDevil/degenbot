from degenbot.uniswap.v4_libraries import full_math, sqrt_price_math

MAX_SWAP_FEE = 1 * 10**6


# @notice Computes the sqrt price target for the next swap step
# @param zeroForOne The direction of the swap, true for currency0 to currency1, False for currency1 to currency0
# @param sqrtPriceNextX96 The Q64.96 sqrt price for the next initialized tick
# @param sqrtPriceLimitX96 The Q64.96 sqrt price limit. If zero for one, the price cannot be less than this value
# after the swap. If one for zero, the price cannot be greater than this value after the swap
# @return sqrtPriceTargetX96 The price target for the next swap step
def get_sqrt_price_target(
    zero_for_one: bool,
    """
    Computes the price target for the next swap step.

    @dev This simplified implementation replicates the gas optimized Yul used by the Solidity
    contract.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/SwapMath.sol
    """

    return (
        max(sqrt_price_next_x96, sqrt_price_limit_x96)
        if zero_for_one
        else min(sqrt_price_next_x96, sqrt_price_limit_x96)
    )
def compute_swap_step(
    sqrt_ratio_x96_current: int,
    sqrt_ratio_x96_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    zero_for_one = sqrt_ratio_x96_current >= sqrt_ratio_x96_target
    exact_in = amount_remaining < 0

    if exact_in:
        amount_remaining_less_fee = full_math.muldiv(
            -amount_remaining, MAX_SWAP_FEE - fee_pips, MAX_SWAP_FEE
        )
        amount_in = (
            sqrt_price_math.get_amount0_delta(
                sqrt_ratio_x96_target, sqrt_ratio_x96_current, liquidity, True
            )
            if zero_for_one
            else sqrt_price_math.get_amount1_delta(
                sqrt_ratio_x96_current, sqrt_ratio_x96_target, liquidity, True
            )
        )
        if amount_remaining_less_fee >= amount_in:
            # `amountIn` is capped by the target price
            sqrt_price_next_x96 = sqrt_ratio_x96_target
            fee_amount = (
                amount_in  # amountIn is always 0 here, as amountRemainingLessFee == 0 and amountRemainingLessFee >= amountIn
                if fee_pips == MAX_SWAP_FEE
                else full_math.muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)
            )
        else:
            # exhaust the remaining amount
            amount_in = amount_remaining_less_fee
            sqrt_price_next_x96 = sqrt_price_math.get_next_sqrt_price_from_input(
                sqrt_ratio_x96_current, liquidity, amount_remaining_less_fee, zero_for_one
            )
            # we didn't reach the target, so take the remainder of the maximum input as fee
            fee_amount = -amount_remaining - amount_in

        amount_out = (
            sqrt_price_math.get_amount1_delta(
                sqrt_price_next_x96, sqrt_ratio_x96_current, liquidity, False
            )
            if zero_for_one
            else sqrt_price_math.get_amount0_delta(
                sqrt_ratio_x96_current, sqrt_price_next_x96, liquidity, False
            )
        )
    else:
        amount_out = (
            sqrt_price_math.get_amount1_delta(
                sqrt_ratio_x96_target, sqrt_ratio_x96_current, liquidity, False
            )
            if zero_for_one
            else sqrt_price_math.get_amount0_delta(
                sqrt_ratio_x96_current, sqrt_ratio_x96_target, liquidity, False
            )
        )
        if amount_remaining >= amount_out:
            # `amountOut` is capped by the target price
            sqrt_price_next_x96 = sqrt_ratio_x96_target
        else:
            # cap the output amount to not exceed the remaining output amount
            amount_out = amount_remaining
            sqrt_price_next_x96 = sqrt_price_math.get_next_sqrt_price_from_output(
                sqrt_ratio_x96_current, liquidity, amount_out, zero_for_one
            )

        amount_in = (
            sqrt_price_math.get_amount0_delta(
                sqrt_price_next_x96, sqrt_ratio_x96_current, liquidity, True
            )
            if zero_for_one
            else sqrt_price_math.get_amount1_delta(
                sqrt_ratio_x96_current, sqrt_price_next_x96, liquidity, True
            )
        )
        # `feePips` cannot be `MAX_SWAP_FEE` for exact out
        fee_amount = full_math.muldiv_rounding_up(amount_in, fee_pips, MAX_SWAP_FEE - fee_pips)

    return sqrt_price_next_x96, amount_in, amount_out, fee_amount
