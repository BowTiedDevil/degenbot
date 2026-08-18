"""
Port of UniswapV3 SqrtPriceMath.sol.

Source: contracts/libraries/SqrtPriceMath.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/SqrtPriceMath.sol

Functions based on Q64.96 sqrt price and liquidity to compute price deltas
and amount deltas. These are the core math primitives that V3's swap
execution uses instead of the V2 constant-product formula.
"""

from . import  full_math
from . import  unsafe_math
from . import  safe_cast
from . import  fixed_point_96


# ═══════════════════════════════════════════════════════════════════════════
# getNextSqrtPrice
# ═══════════════════════════════════════════════════════════════════════════


@internal
@pure
def get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_px96: uint160, liquidity: uint128, amount: uint256, add: bool
) -> uint160:
    """Gets the next sqrt price given a delta of token0.

    Always rounds up: in the exact output case (increasing price) we need to
    move the price at least far enough to get the desired output amount, and
    in the exact input case (decreasing price) we need to move the price
    less in order to not send too much output.

    The most precise formula: liquidity * sqrtPX96 / (liquidity +- amount * sqrtPX96).
    If overflow, falls back to: liquidity / (liquidity / sqrtPX96 +- amount).
    """
    if amount == 0:
        return sqrt_px96

    numerator1: uint256 = convert(liquidity, uint256) << fixed_point_96.RESOLUTION

    if add:
        product: uint256 = unsafe_mul(amount, convert(sqrt_px96, uint256))
        # Check if product didn't overflow (in Solidity: product / amount == sqrtPX96)
        if product // amount == convert(sqrt_px96, uint256):
            denominator: uint256 = unsafe_add(numerator1, product)
            if denominator >= numerator1:
                return safe_cast.to_uint160(
                    full_math.mul_div_rounding_up(numerator1, convert(sqrt_px96, uint256), denominator)
                )

        # Overflow path: liquidity / (liquidity / sqrtPX96 + amount)
        return safe_cast.to_uint160(
            unsafe_math.div_rounding_up(
                numerator1,
                unsafe_add(numerator1 // convert(sqrt_px96, uint256), amount)
            )
        )
    else:
        # Subtract path: check product doesn't overflow, and numerator1 > product
        product: uint256 = unsafe_mul(amount, convert(sqrt_px96, uint256))
        assert product // amount == convert(sqrt_px96, uint256) and numerator1 > product
        denominator: uint256 = numerator1 - product
        return safe_cast.to_uint160(
            full_math.mul_div_rounding_up(numerator1, convert(sqrt_px96, uint256), denominator)
        )


@internal
@pure
def get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_px96: uint160, liquidity: uint128, amount: uint256, add: bool
) -> uint160:
    """Gets the next sqrt price given a delta of token1.

    Always rounds down: in the exact output case (decreasing price) we need
    to move the price at least far enough, and in the exact input case
    (increasing price) we need to move it less to not send too much output.

    Formula: sqrtPX96 +- amount / liquidity (within <1 wei of lossless).
    """
    if add:
        # Rounding down for addition: round quotient down
        quotient: uint256 = empty(uint256)
        if amount <= convert(max_value(uint160), uint256):
            quotient = (amount << fixed_point_96.RESOLUTION) // convert(liquidity, uint256)
        else:
            quotient = full_math.mul_div(amount, fixed_point_96.Q96, convert(liquidity, uint256))

        return safe_cast.to_uint160(unsafe_add(convert(sqrt_px96, uint256), quotient))
    else:
        # Rounding up for subtraction: round quotient up
        quotient: uint256 = empty(uint256)
        if amount <= convert(max_value(uint160), uint256):
            quotient = unsafe_math.div_rounding_up(amount << fixed_point_96.RESOLUTION, convert(liquidity, uint256))
        else:
            quotient = full_math.mul_div_rounding_up(amount, fixed_point_96.Q96, convert(liquidity, uint256))

        assert convert(sqrt_px96, uint256) > quotient
        return convert(convert(sqrt_px96, uint256) - quotient, uint160)


@internal
@pure
def get_next_sqrt_price_from_input(
    sqrt_px96: uint160, liquidity: uint128, amount_in: uint256, zero_for_one: bool
) -> uint160:
    """Gets the next sqrt price given an input amount of token0 or token1.

    Rounds so that we don't pass the target price.
    """
    assert sqrt_px96 > 0
    assert convert(liquidity, uint256) > 0

    if zero_for_one:
        return self.get_next_sqrt_price_from_amount0_rounding_up(sqrt_px96, liquidity, amount_in, True)
    else:
        return self.get_next_sqrt_price_from_amount1_rounding_down(sqrt_px96, liquidity, amount_in, True)


@internal
@pure
def get_next_sqrt_price_from_output(
    sqrt_px96: uint160, liquidity: uint128, amount_out: uint256, zero_for_one: bool
) -> uint160:
    """Gets the next sqrt price given an output amount of token0 or token1.

    Rounds so that we pass the target price.
    """
    assert sqrt_px96 > 0
    assert convert(liquidity, uint256) > 0

    if zero_for_one:
        return self.get_next_sqrt_price_from_amount1_rounding_down(sqrt_px96, liquidity, amount_out, False)
    else:
        return self.get_next_sqrt_price_from_amount0_rounding_up(sqrt_px96, liquidity, amount_out, False)


# ═══════════════════════════════════════════════════════════════════════════
# getAmountDelta
# ═══════════════════════════════════════════════════════════════════════════


@internal
@pure
def get_amount0_delta(
    sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, liquidity: uint128, round_up: bool
) -> uint256:
    """Gets the amount0 delta between two prices.

    Calculates: liquidity / sqrt(lower) - liquidity / sqrt(upper)
    i.e. liquidity * (sqrt(upper) - sqrt(lower)) / (sqrt(upper) * sqrt(lower))
    """
    if sqrt_ratio_ax96 > sqrt_ratio_bx96:
        _tmp: uint160 = sqrt_ratio_ax96
        sqrt_ratio_ax96 = sqrt_ratio_bx96
        sqrt_ratio_bx96 = _tmp

    numerator1: uint256 = convert(liquidity, uint256) << fixed_point_96.RESOLUTION
    numerator2: uint256 = convert(sqrt_ratio_bx96, uint256) - convert(sqrt_ratio_ax96, uint256)

    assert convert(sqrt_ratio_ax96, uint256) > 0

    if round_up:
        return unsafe_math.div_rounding_up(
            full_math.mul_div_rounding_up(numerator1, numerator2, convert(sqrt_ratio_bx96, uint256)),
            convert(sqrt_ratio_ax96, uint256)
        )
    else:
        return full_math.mul_div(numerator1, numerator2, convert(sqrt_ratio_bx96, uint256)) // convert(sqrt_ratio_ax96, uint256)


@internal
@pure
def get_amount1_delta(
    sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, liquidity: uint128, round_up: bool
) -> uint256:
    """Gets the amount1 delta between two prices.

    Calculates: liquidity * (sqrt(upper) - sqrt(lower))
    """
    if sqrt_ratio_ax96 > sqrt_ratio_bx96:
        _tmp2: uint160 = sqrt_ratio_ax96
        sqrt_ratio_ax96 = sqrt_ratio_bx96
        sqrt_ratio_bx96 = _tmp2

    if round_up:
        return full_math.mul_div_rounding_up(
            convert(liquidity, uint256),
            convert(sqrt_ratio_bx96, uint256) - convert(sqrt_ratio_ax96, uint256),
            fixed_point_96.Q96
        )
    else:
        return full_math.mul_div(
            convert(liquidity, uint256),
            convert(sqrt_ratio_bx96, uint256) - convert(sqrt_ratio_ax96, uint256),
            fixed_point_96.Q96
        )


@internal
@pure
def get_amount0_delta_signed(
    sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, _liquidity: int128
) -> int256:
    """Helper that gets signed token0 delta."""
    if _liquidity < 0:
        return -safe_cast.to_int256(
            self.get_amount0_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, convert(-_liquidity, uint128), False)
        )
    else:
        return safe_cast.to_int256(
            self.get_amount0_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, convert(_liquidity, uint128), True)
        )


@internal
@pure
def get_amount1_delta_signed(
    sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, _liquidity: int128
) -> int256:
    """Helper that gets signed token1 delta."""
    if _liquidity < 0:
        return -safe_cast.to_int256(
            self.get_amount1_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, convert(-_liquidity, uint128), False)
        )
    else:
        return safe_cast.to_int256(
            self.get_amount1_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, convert(_liquidity, uint128), True)
        )
