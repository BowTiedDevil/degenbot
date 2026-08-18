"""
Test harness that exposes library functions for verification.

Deploy this contract in tests, then call the @external wrappers
to compare against Solidity reference values from `chisel eval`.
"""

from .libraries import full_math
from .libraries import unsafe_math
from .libraries import safe_cast
from .libraries import sqrt_price_math
from .libraries import tick_math
from .libraries import swap_math


# ═══════════════════════════════════════════════════════════════════════════
# FullMath
# ═══════════════════════════════════════════════════════════════════════════


@external
@pure
def test_mul_div(a: uint256, b: uint256, d: uint256) -> uint256:
    return full_math.mul_div(a, b, d)


@external
@pure
def test_mul_div_rounding_up(a: uint256, b: uint256, d: uint256) -> uint256:
    return full_math.mul_div_rounding_up(a, b, d)


# ═══════════════════════════════════════════════════════════════════════════
# UnsafeMath
# ═══════════════════════════════════════════════════════════════════════════


@external
@pure
def test_div_rounding_up(x: uint256, y: uint256) -> uint256:
    return unsafe_math.div_rounding_up(x, y)


# ═══════════════════════════════════════════════════════════════════════════
# SafeCast
# ═══════════════════════════════════════════════════════════════════════════


@external
@pure
def test_to_uint160(y: uint256) -> uint160:
    return safe_cast.to_uint160(y)


@external
@pure
def test_to_int128(y: int256) -> int128:
    return safe_cast.to_int128(y)


@external
@pure
def test_to_int256(y: uint256) -> int256:
    return safe_cast.to_int256(y)


# ═══════════════════════════════════════════════════════════════════════════
# SqrtPriceMath
# ═══════════════════════════════════════════════════════════════════════════


@external
@pure
def test_get_next_sqrt_price_from_input(sqrt_px96: uint160, liquidity: uint128, amount_in: uint256, zero_for_one: bool) -> uint160:
    return sqrt_price_math.get_next_sqrt_price_from_input(sqrt_px96, liquidity, amount_in, zero_for_one)


@external
@pure
def test_get_next_sqrt_price_from_output(sqrt_px96: uint160, liquidity: uint128, amount_out: uint256, zero_for_one: bool) -> uint160:
    return sqrt_price_math.get_next_sqrt_price_from_output(sqrt_px96, liquidity, amount_out, zero_for_one)


@external
@pure
def test_get_amount0_delta(sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, liquidity: uint128, round_up: bool) -> uint256:
    return sqrt_price_math.get_amount0_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, liquidity, round_up)


@external
@pure
def test_get_amount1_delta(sqrt_ratio_ax96: uint160, sqrt_ratio_bx96: uint160, liquidity: uint128, round_up: bool) -> uint256:
    return sqrt_price_math.get_amount1_delta(sqrt_ratio_ax96, sqrt_ratio_bx96, liquidity, round_up)


@external
@pure
def test_get_tick_at_sqrt_ratio(sqrt_price_x96: uint160) -> int24:
    return tick_math.get_tick_at_sqrt_ratio(sqrt_price_x96)


@external
@pure
def test_get_sqrt_ratio_at_tick(tick: int24) -> uint160:
    return tick_math.get_sqrt_ratio_at_tick(tick)


@external
@pure
def test_compute_swap_step(
    sqrt_ratio_current_x96: uint160,
    sqrt_ratio_target_x96: uint160,
    liquidity: uint128,
    amount_remaining: int256,
    fee_pips: uint24,
) -> (uint160, uint256, uint256, uint256):
    return swap_math.compute_swap_step(
        sqrt_ratio_current_x96, sqrt_ratio_target_x96, liquidity, amount_remaining, fee_pips
    )


@external
@payable
def __default__():
    return
