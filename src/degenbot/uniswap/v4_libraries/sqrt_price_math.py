import functools

from degenbot.constants import MAX_UINT160, MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.v4_libraries._config import V4_LIB_CACHE_SIZE
from degenbot.uniswap.v4_libraries.fixed_point_96 import Q96, RESOLUTION
from degenbot.uniswap.v4_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v4_libraries.functions import mulmod
from degenbot.uniswap.v4_libraries.unsafe_math import div_rounding_up


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_amount0_delta(
    *,
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """
    Gets the amount0 delta between two prices
    """

    if round_up is None:
        return (
            get_amount0_delta(
                sqrt_price_a_x96=sqrt_price_a_x96,
                sqrt_price_b_x96=sqrt_price_b_x96,
                liquidity=-liquidity,
                round_up=False,
            )
            if liquidity < 0
            else -get_amount0_delta(
                sqrt_price_a_x96=sqrt_price_a_x96,
                sqrt_price_b_x96=sqrt_price_b_x96,
                liquidity=liquidity,
                round_up=True,
            )
        )

    if sqrt_price_a_x96 > sqrt_price_b_x96:
        sqrt_price_a_x96, sqrt_price_b_x96 = sqrt_price_b_x96, sqrt_price_a_x96

    if sqrt_price_a_x96 == 0:
        msg = "InvalidPrice"
        raise EVMRevertError(msg)

    numerator1 = liquidity << RESOLUTION
    numerator2 = sqrt_price_b_x96 - sqrt_price_a_x96
    return (
        div_rounding_up(
            muldiv_rounding_up(numerator1, numerator2, sqrt_price_b_x96), sqrt_price_a_x96
        )
        if round_up
        else muldiv(numerator1, numerator2, sqrt_price_b_x96) // sqrt_price_a_x96
    )


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_amount1_delta(
    *,
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """
    Gets the amount1 delta between two prices
    """

    if round_up is None:
        return (
            get_amount1_delta(
                sqrt_price_a_x96=sqrt_price_a_x96,
                sqrt_price_b_x96=sqrt_price_b_x96,
                liquidity=-liquidity,
                round_up=False,
            )
            if liquidity < 0
            else -get_amount1_delta(
                sqrt_price_a_x96=sqrt_price_a_x96,
                sqrt_price_b_x96=sqrt_price_b_x96,
                liquidity=liquidity,
                round_up=True,
            )
        )

    numerator = abs(sqrt_price_a_x96 - sqrt_price_b_x96)
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


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount0_rounding_up(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
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
        if product <= MAX_UINT256:  # product did not overflow
            denominator = numerator1 + product
            if denominator >= numerator1:
                # always fits in 160 bits
                return muldiv_rounding_up(
                    a=numerator1,
                    b=sqrt_price_x96,
                    denominator=denominator,
                )

        # product overflowed
        return div_rounding_up(
            x=numerator1,
            y=(numerator1 // sqrt_price_x96) + amount,
        )

    # equivalent: if (product / amount != sqrtPX96 || numerator1 <= product) revert PriceOverflow();
    if product // amount != sqrt_price_x96 or numerator1 <= product:
        msg = "PriceOverflow"
        raise EVMRevertError(msg)

    result = muldiv_rounding_up(
        a=numerator1,
        b=sqrt_price_x96,
        denominator=numerator1 - product,
    )
    if result > MAX_UINT160:
        msg = "Safecast Overflowed: uint160"
        raise EVMRevertError(msg)

    return result


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount1_rounding_down(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
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
        result = sqrt_price_x96 + quotient
        if result > MAX_UINT160:
            msg = "Result overflowed MAX_UINT160"
            raise EVMRevertError(msg)
        return result

    quotient = (
        div_rounding_up(amount << RESOLUTION, liquidity)
        if amount <= MAX_UINT160
        else muldiv_rounding_up(amount, Q96, liquidity)
    )

    if sqrt_price_x96 <= quotient:
        raise EVMRevertError(error="NotEnoughLiquidity")

    # always fits 160 bits
    return sqrt_price_x96 - quotient


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_input(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """
    Gets the next sqrt price given an input amount of currency0 or currency1, rounding to ensure
    that the target price is not passed.
    """

    if sqrt_price_x96 == 0 or liquidity == 0:
        msg = "InvalidPriceOrLiquidity"
        raise EVMRevertError(msg)

    return (
        get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            amount=amount_in,
            add=True,
        )
        if zero_for_one
        else get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            amount=amount_in,
            add=True,
        )
    )


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_output(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """
    Gets the next sqrt price given an output amount of currency0 or currency1, rounding to ensure
    that the target price is not passed.
    """

    if sqrt_price_x96 == 0 or liquidity == 0:
        msg = "InvalidPriceOrLiquidity"
        raise EVMRevertError(msg)

    return (
        get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            amount=amount_out,
            add=False,
        )
        if zero_for_one
        else get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            amount=amount_out,
            add=False,
        )
    )
