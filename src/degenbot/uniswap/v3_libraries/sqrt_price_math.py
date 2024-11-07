from functools import lru_cache

from degenbot.constants import MIN_UINT160
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries.constants import Q96, Q96_RESOLUTION
from degenbot.uniswap.v3_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v3_libraries.functions import to_int256, to_uint160
from degenbot.uniswap.v3_libraries.unsafe_math import div_rounding_up


@lru_cache
def get_amount0_delta(
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    # The Solidity function is overloaded with respect to `roundUp`.
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/sqrt_price_math.sol

    if round_up is not None:
        if sqrt_ratio_a_x96 > sqrt_ratio_b_x96:
            sqrt_ratio_a_x96, sqrt_ratio_b_x96 = sqrt_ratio_b_x96, sqrt_ratio_a_x96

        numerator1 = liquidity << Q96_RESOLUTION
        numerator2 = sqrt_ratio_b_x96 - sqrt_ratio_a_x96

        if not (sqrt_ratio_a_x96 > 0):
            raise EVMRevertError(error="required: sqrt_ratio_a_x96 > 0")

        return (
            div_rounding_up(
                muldiv_rounding_up(numerator1, numerator2, sqrt_ratio_b_x96),
                sqrt_ratio_a_x96,
            )
            if round_up
            else muldiv(numerator1, numerator2, sqrt_ratio_b_x96) // sqrt_ratio_a_x96
        )

    return to_int256(
        to_int256(-get_amount0_delta(sqrt_ratio_a_x96, sqrt_ratio_b_x96, -liquidity, False))
        if liquidity < 0
        else to_int256(get_amount0_delta(sqrt_ratio_a_x96, sqrt_ratio_b_x96, liquidity, True))
    )


@lru_cache
def get_amount1_delta(
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    # The Solidity function is overloaded with respect to `roundUp`.
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/sqrt_price_math.sol

    if round_up is not None:
        if sqrt_ratio_a_x96 > sqrt_ratio_b_x96:
            sqrt_ratio_a_x96, sqrt_ratio_b_x96 = sqrt_ratio_b_x96, sqrt_ratio_a_x96

        return (
            muldiv_rounding_up(liquidity, sqrt_ratio_b_x96 - sqrt_ratio_a_x96, Q96)
            if round_up
            else muldiv(liquidity, sqrt_ratio_b_x96 - sqrt_ratio_a_x96, Q96)
        )

    return to_int256(
        to_int256(-get_amount1_delta(sqrt_ratio_a_x96, sqrt_ratio_b_x96, -liquidity, False))
        if liquidity < 0
        else to_int256(get_amount1_delta(sqrt_ratio_a_x96, sqrt_ratio_b_x96, liquidity, True))
    )


@lru_cache
def get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    if amount == 0:
        return sqrt_price_x96

    numerator1 = liquidity << Q96_RESOLUTION

    if add:
        return (
            muldiv_rounding_up(numerator1, sqrt_price_x96, denominator)
            if (
                (product := amount * sqrt_price_x96) // amount == sqrt_price_x96
                and (denominator := numerator1 + product) >= numerator1
            )
            else div_rounding_up(numerator1, numerator1 // sqrt_price_x96 + amount)
        )

    product = amount * sqrt_price_x96
    if not (product // amount == sqrt_price_x96 and numerator1 > product):
        raise EVMRevertError(error="required: product // amount == sqrtPX96, numerator1 > product")

    denominator = numerator1 - product
    return to_uint160(muldiv_rounding_up(numerator1, sqrt_price_x96, denominator))


@lru_cache
def get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    if add:
        quotient = (
            (amount << Q96_RESOLUTION) // liquidity
            if amount <= 2**160 - 1
            else muldiv(amount, Q96, liquidity)
        )
        return to_uint160(sqrt_price_x96 + quotient)

    quotient = (
        div_rounding_up(amount << Q96_RESOLUTION, liquidity)
        if amount <= (2**160) - 1
        else muldiv_rounding_up(amount, Q96, liquidity)
    )

    if not (sqrt_price_x96 > quotient):
        raise EVMRevertError(error="require sqrtPX96 > quotient")

    # always fits 160 bits
    return sqrt_price_x96 - quotient


@lru_cache
def get_next_sqrt_price_from_input(
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    if not (sqrt_price_x96 > MIN_UINT160):
        raise EVMRevertError(error="required: sqrt_price_x96 > 0")

    if not (liquidity > MIN_UINT160):
        raise EVMRevertError(error="required: liquidity > 0")

    # round to make sure that we don't pass the target price
    return (
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_price_x96, liquidity, amount_in, True)
        if zero_for_one
        else get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96, liquidity, amount_in, True
        )
    )


@lru_cache
def get_next_sqrt_price_from_output(
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    if not (sqrt_price_x96 > 0):
        raise EVMRevertError(error="required: sqrt_price_x96 > 0")

    if not (liquidity > 0):
        raise EVMRevertError(error="required: liquidity must be > 0")

    # round to make sure that we pass the target price
    return (
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_price_x96, liquidity, amount_out, False)
        if zero_for_one
        else get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96, liquidity, amount_out, False
        )
    )
