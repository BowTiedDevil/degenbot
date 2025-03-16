from degenbot.constants import MAX_INT256, MAX_UINT160, MAX_UINT256, MIN_INT256, MIN_UINT160
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v4_libraries.fixed_point_96 import Q96, RESOLUTION
from degenbot.uniswap.v4_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v4_libraries.functions import mulmod
from degenbot.uniswap.v4_libraries.unsafe_math import div_rounding_up


# @notice Gets the next sqrt price given a delta of currency0
# @dev Always rounds up, because in the exact output case (increasing price) we need to move the price at least
# far enough to get the desired output amount, and in the exact input case (decreasing price) we need to move the
# price less in order to not send too much output.
# The most precise formula for this is liquidity * sqrtPX96 / (liquidity +- amount * sqrtPX96),
# if this is impossible because of overflow, we calculate liquidity / (liquidity / sqrtPX96 +- amount).
# @param sqrtPX96 The starting price, i.e. before accounting for the currency0 delta
# @param liquidity The amount of usable liquidity
# @param amount How much of currency0 to add or remove from virtual reserves
# @param add Whether to add or remove the amount of currency0
# @return The price after adding or removing amount, depending on add
def get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    # we short circuit amount == 0 because the result is otherwise not guaranteed to equal the input price
    if amount == 0:
        return sqrt_price_x96

    numerator1: int = liquidity << RESOLUTION
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

    # if the product overflows, we know the denominator underflows
    # in addition, we must check that the denominator does not underflow
    # equivalent: if (product / amount != sqrtPX96 || numerator1 <= product) revert PriceOverflow();
    if (
        (
            (0 if amount == 0 else product // amount)
            == (sqrt_price_x96 & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF)
        )
        & (numerator1 > product)
    ) == 0:
        raise EVMRevertError(error="PriceOverflow")

    denominator = numerator1 - product
    result = muldiv_rounding_up(numerator1, sqrt_price_x96, denominator)
    if not (MIN_UINT160 <= result <= MAX_UINT160):
        raise EVMRevertError(error="SafeCastOverflow")
    return result


# @notice Gets the next sqrt price given a delta of currency1
# @dev Always rounds down, because in the exact output case (decreasing price) we need to move the price at least
# far enough to get the desired output amount, and in the exact input case (increasing price) we need to move the
# price less in order to not send too much output.
# The formula we compute is within <1 wei of the lossless version: sqrtPX96 +- amount / liquidity
# @param sqrtPX96 The starting price, i.e., before accounting for the currency1 delta
# @param liquidity The amount of usable liquidity
# @param amount How much of currency1 to add, or remove, from virtual reserves
# @param add Whether to add, or remove, the amount of currency1
# @return The price after adding or removing `amount`
def get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    # if we're adding (subtracting), rounding down requires rounding the quotient down (up)
    # in both cases, avoid a mulDiv for most inputs
    if add:
        quotient = (
            (amount << RESOLUTION) // liquidity
            if amount <= MAX_UINT160
            else muldiv(amount, Q96, liquidity)
        )
        result = sqrt_price_x96 + quotient
        if not (MIN_UINT160 <= result <= MAX_UINT160):
            raise EVMRevertError(error="SafeCastOverflow")
        return result

    quotient = (
        div_rounding_up(amount << RESOLUTION, liquidity)
        if amount <= MAX_UINT160
        else muldiv_rounding_up(amount, Q96, liquidity)
    )
    # equivalent: if (sqrtPX96 <= quotient) revert NotEnoughLiquidity();

    if (sqrt_price_x96 & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF > quotient) == 0:
        raise EVMRevertError(error="NotEnoughLiquidity")

    # always fits 160 bits
    return sqrt_price_x96 - quotient


# @notice Gets the next sqrt price given an input amount of currency0 or currency1
# @dev Throws if price or liquidity are 0, or if the next price is out of bounds
# @param sqrtPX96 The starting price, i.e., before accounting for the input amount
# @param liquidity The amount of usable liquidity
# @param amountIn How much of currency0, or currency1, is being swapped in
# @param zeroForOne Whether the amount in is currency0 or currency1
# @return uint160 The price after adding the input amount to currency0 or currency1
def get_next_sqrt_price_from_input(
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    # equivalent: if (sqrtPX96 == 0 || liquidity == 0) revert InvalidPriceOrLiquidity();
    if ((sqrt_price_x96 & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF) == 0) or (
        (liquidity & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF) == 0
    ):
        raise EVMRevertError(error="InvalidPriceOrLiquidity")

    # round to make sure that we don't pass the target price
    return (
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_in, True)
        if zero_for_one
        else get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96, liquidity, amount_in, True
        )
    )


# @notice Gets the next sqrt price given an output amount of currency0 or currency1
# @dev Throws if price or liquidity are 0 or the next price is out of bounds
# @param sqrtPX96 The starting price before accounting for the output amount
# @param liquidity The amount of usable liquidity
# @param amountOut How much of currency0, or currency1, is being swapped out
# @param zeroForOne Whether the amount out is currency1 or currency0
# @return uint160 The price after removing the output amount of currency0 or currency1
def get_next_sqrt_price_from_output(
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    # equivalent: if (sqrtPX96 == 0 || liquidity == 0) revert InvalidPriceOrLiquidity();

    if ((sqrt_price_x96 & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF) == 0) or (
        (liquidity & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF) == 0
    ):
        raise EVMRevertError(error="InvalidPriceOrLiquidity")

    # round to make sure that we pass the target price
    return (
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_out, False)
        if zero_for_one
        else get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96, liquidity, amount_out, False
        )
    )


# @notice Equivalent to: `a >= b ? a - b : b - a`
def abs_diff(a: int, b: int) -> int:
    """
    Calculate the absolute difference between two values.

    This implementation replaces the Solidity version which uses inline Yul in place of abs()

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/SqrtPriceMath.sol
    """

    return abs(a - b)


# @notice Gets the amount0 delta between two prices
# @dev Calculates liquidity / sqrt(lower) - liquidity / sqrt(upper),
# i.e. liquidity * (sqrt(upper) - sqrt(lower)) / (sqrt(upper) * sqrt(lower))
# @param sqrtPriceAX96 A sqrt price
# @param sqrtPriceBX96 Another sqrt price
# @param liquidity The amount of usable liquidity
# @param roundUp Whether to round the amount up or down
# @return uint256 Amount of currency0 required to cover a position of size liquidity between the two passed prices
def get_amount0_delta(
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    if round_up is None:
        result = (
            get_amount0_delta(sqrt_price_a_x96, sqrt_price_b_x96, -liquidity, False)
            if liquidity < 0
            else -get_amount0_delta(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, True)
        )
        if not (MIN_INT256 <= result <= MAX_INT256):
            raise EVMRevertError(error="SafeCastOverflow")
        return result

    if sqrt_price_a_x96 > sqrt_price_b_x96:
        sqrt_price_a_x96, sqrt_price_b_x96 = sqrt_price_b_x96, sqrt_price_a_x96

    # equivalent: if (sqrtPriceAX96 == 0) revert InvalidPrice();

    if (sqrt_price_a_x96 & 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF) == 0:
        raise EVMRevertError(error="InvalidPrice")

    numerator1 = liquidity << RESOLUTION
    numerator2 = sqrt_price_b_x96 - sqrt_price_a_x96
    return (
        div_rounding_up(
            muldiv_rounding_up(numerator1, numerator2, sqrt_price_b_x96), sqrt_price_a_x96
        )
        if round_up
        else muldiv(numerator1, numerator2, sqrt_price_b_x96) // sqrt_price_a_x96
    )


# @notice Gets the amount1 delta between two prices
# @dev Calculates liquidity * (sqrt(upper) - sqrt(lower))
# @param sqrtPriceAX96 A sqrt price
# @param sqrtPriceBX96 Another sqrt price
# @param liquidity The amount of usable liquidity
# @param roundUp Whether to round the amount up, or down
# @return amount1 Amount of currency1 required to cover a position of size liquidity between the two passed prices
def get_amount1_delta(
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    if round_up is None:
        result = (
            get_amount1_delta(sqrt_price_a_x96, sqrt_price_b_x96, -liquidity, False)
            if liquidity < 0
            else -get_amount1_delta(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, True)
        )
        if not (MIN_INT256 <= result <= MAX_INT256):
            raise EVMRevertError(error="SafeCastOverflow")
        return result

    numerator = abs_diff(sqrt_price_a_x96, sqrt_price_b_x96)
    denominator = Q96
    _liquidity = liquidity
    # Equivalent to:
    #   amount1 = roundUp
    #       ? FullMath.mulDivRoundingUp(liquidity, sqrtPriceBX96 - sqrtPriceAX96, FixedPoint96.Q96)
    #       : FullMath.mulDiv(liquidity, sqrtPriceBX96 - sqrtPriceAX96, FixedPoint96.Q96);
    # Cannot overflow because `type(uint128).max * type(uint160).max >> 96 < (1 << 192)`.
    #
    amount1 = muldiv(_liquidity, numerator, denominator)
    amount1 = amount1 + ((mulmod(_liquidity, numerator, denominator) > 0) & round_up)
    return amount1
