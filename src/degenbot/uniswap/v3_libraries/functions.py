"""Uniswap V3 pure helper functions for tick and price conversion."""
from degenbot.constants import MAX_INT128, MAX_INT256, MAX_UINT160, MIN_INT128, MIN_INT256
from degenbot.exceptions.pool import EVMRevertError


def mulmod(x: int, y: int, k: int) -> int:
    """Return mulmod."""
    if k == 0:
        raise EVMRevertError(error="division by zero")
    return (x * y) % k


# adapted from OpenZeppelin's overflow checks, which throw
# an exception if the input value exceeds the maximum value
# for this type
def to_int128(x: int) -> int:
    """Convert to int128."""
    if not (MIN_INT128 <= x <= MAX_INT128):
        raise EVMRevertError(error=f"{x} outside range of int128 values")
    return x


def to_int256(x: int) -> int:
    """Convert to int256."""
    if not (MIN_INT256 <= x <= MAX_INT256):
        raise EVMRevertError(error=f"{x} outside range of int256 values")
    return x


def to_uint160(x: int) -> int:
    """Convert to uint160."""
    if x > MAX_UINT160:
        raise EVMRevertError(error=f"{x} greater than maximum uint160 value")
    return x


Q96 = 2**96


def v3_virtual_reserves(
    liquidity: int,
    sqrt_price_x96: int,
    *,
    zero_for_one: bool,
) -> tuple[int, int]:
    """V3 virtual reserves."""
    x_virtual = liquidity * Q96 * Q96 // sqrt_price_x96
    y_virtual = liquidity * sqrt_price_x96
    if zero_for_one:
        return x_virtual, y_virtual
    return y_virtual, x_virtual
