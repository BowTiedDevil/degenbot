from pydantic import validate_call

from degenbot.constants import MAX_UINT160, MAX_UINT256, MIN_UINT160
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v4_libraries.fixed_point_96 import Q96, RESOLUTION
from degenbot.uniswap.v4_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v4_libraries.functions import mulmod
from degenbot.uniswap.v4_libraries.unsafe_math import div_rounding_up
from degenbot.validation.evm_values import (
    ValidatedUint128,
    ValidatedUint128NonZero,
    ValidatedUint160,
    ValidatedUint160NonZero,
    ValidatedUint256,
)


@validate_call(validate_return=True)
def get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: ValidatedUint160,
    liquidity: ValidatedUint128,
    amount: ValidatedUint256,
    add: bool,
) -> ValidatedUint160:
    """
    Gets the next sqrt price given a delta of currency0
    """

    # Short circuit amount == 0 because the result is otherwise not guaranteed to equal the input
    # price
    if amount == 0:
        return sqrt_price_x96

    numerator1 = liquidity << RESOLUTION
    product = amount * sqrt_price_x96

    # @dev the Solidity contract uses an unchecked math block to determine if overflow has occured.
    # Python integer math cannot overflow, so check directly if the numerator exceeds the uint256
    # upper bound
    if add:
        if product < MAX_UINT256:  # product did not overflow
            denominator = numerator1 + product
            if denominator >= numerator1:
                # always fits in 160 bits
                return muldiv_rounding_up(
                    a=numerator1,
                    b=sqrt_price_x96,
                    denominator=denominator,
                )

        # product overflowed - failsafe path
        result = div_rounding_up(
            x=numerator1,
            y=(numerator1 // sqrt_price_x96) + amount,
        )
        assert MIN_UINT160 <= result <= MAX_UINT160
        return result

    # @dev: removed overflow check on denominator in favor of input validation on muldiv_rounding_up
    denominator = numerator1 - product
    return muldiv_rounding_up(numerator1, sqrt_price_x96, denominator)


@validate_call(validate_return=True)
def get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: ValidatedUint160,
    liquidity: ValidatedUint128,
    amount: ValidatedUint256,
    add: bool,
) -> ValidatedUint160:
    """
    Gets the next sqrt price given a delta of currency1
    """

    # if we're adding (subtracting), rounding down requires rounding the quotient down (up)
    # in both cases, avoid a mulDiv for most inputs
    if add:
        quotient = (
            (amount << RESOLUTION) // liquidity
            if amount <= MAX_UINT160
            else muldiv(amount, Q96, liquidity)
        )
        return sqrt_price_x96 + quotient

    quotient = (
        div_rounding_up(amount << RESOLUTION, liquidity)
        if amount <= MAX_UINT160
        else muldiv_rounding_up(amount, Q96, liquidity)
    )

    if sqrt_price_x96 <= quotient:
        raise EVMRevertError(error="NotEnoughLiquidity")

    # always fits 160 bits
    return sqrt_price_x96 - quotient


@validate_call(validate_return=True)
def get_next_sqrt_price_from_input(
    sqrt_price_x96: ValidatedUint160NonZero,
    liquidity: ValidatedUint128NonZero,
    amount_in: ValidatedUint256,
    zero_for_one: bool,
) -> ValidatedUint160:
    """
    Gets the next sqrt price given an input amount of currency0 or currency1, rounding to ensure
    that the target price is not passed.
    """

    return (
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_in, True)
        if zero_for_one
        else get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96, liquidity, amount_in, True
        )
    )


@validate_call(validate_return=True)
def get_next_sqrt_price_from_output(
    sqrt_price_x96: ValidatedUint160NonZero,
    liquidity: ValidatedUint128NonZero,
    amount_out: ValidatedUint256,
    zero_for_one: bool,
) -> ValidatedUint160:
    """
    Gets the next sqrt price given an output amount of currency0 or currency1, rounding to ensure
    that the target price is not passed.
    """

    return (
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_out, False)
        if zero_for_one
        else get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96, liquidity, amount_out, False
        )
    )


@validate_call(validate_return=True)
def abs_diff(
    a: ValidatedUint160,
    b: ValidatedUint160,
) -> ValidatedUint256:
    """
    Calculate the absolute difference between two values.

    This implementation replaces the Solidity version which uses inline Yul.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/SqrtPriceMath.sol
    """

    return a - b if a >= b else b - a


@validate_call(validate_return=True)
def get_amount0_delta(
    sqrt_price_a_x96: ValidatedUint160NonZero,
    sqrt_price_b_x96: ValidatedUint160,
    liquidity: ValidatedUint128,
    round_up: bool | None = None,
) -> ValidatedUint256:
    """
    Gets the amount0 delta between two prices
    """

    if round_up is None:
        return (
            get_amount0_delta(sqrt_price_a_x96, sqrt_price_b_x96, -liquidity, False)
            if liquidity < 0
            else -get_amount0_delta(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, True)
        )

    if sqrt_price_a_x96 > sqrt_price_b_x96:
        sqrt_price_a_x96, sqrt_price_b_x96 = sqrt_price_b_x96, sqrt_price_a_x96

    numerator1 = liquidity << RESOLUTION
    numerator2 = sqrt_price_b_x96 - sqrt_price_a_x96
    return (
        div_rounding_up(
            muldiv_rounding_up(numerator1, numerator2, sqrt_price_b_x96), sqrt_price_a_x96
        )
        if round_up
        else muldiv(numerator1, numerator2, sqrt_price_b_x96) // sqrt_price_a_x96
    )


@validate_call(validate_return=True)
def get_amount1_delta(
    sqrt_price_a_x96: ValidatedUint160,
    sqrt_price_b_x96: ValidatedUint160,
    liquidity: ValidatedUint128,
    round_up: bool | None = None,
) -> ValidatedUint256:
    """
    Gets the amount1 delta between two prices
    """

    if round_up is None:
        return (
            get_amount1_delta(sqrt_price_a_x96, sqrt_price_b_x96, -liquidity, False)
            if liquidity < 0
            else -get_amount1_delta(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, True)
        )

    numerator = abs_diff(sqrt_price_a_x96, sqrt_price_b_x96)
    denominator = Q96
    _liquidity = liquidity
    # Equivalent to:
    # ... amount1 = roundUp
    #       ? FullMath.mulDivRoundingUp(liquidity, sqrtPriceBX96 - sqrtPriceAX96, FixedPoint96.Q96)
    #       : FullMath.mulDiv(liquidity, sqrtPriceBX96 - sqrtPriceAX96, FixedPoint96.Q96);
    # Cannot overflow because `type(uint128).max * type(uint160).max >> 96 < (1 << 192)`.
    return muldiv(_liquidity, numerator, denominator) + (
        int(mulmod(_liquidity, numerator, denominator) > 0) & round_up
    )
