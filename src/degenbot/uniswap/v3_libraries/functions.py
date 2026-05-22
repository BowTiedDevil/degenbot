"""Uniswap V3 pure helper functions for tick and price conversion."""
from degenbot.constants import MAX_INT128, MAX_INT256, MAX_UINT160, MIN_INT128, MIN_INT256
from degenbot.exceptions.pool import EVMRevertError


def mulmod(x: int, y: int, k: int) -> int:
    """Return mulmod.

    Returns:
        The result of (x * y) mod k.

    Raises:
        EVMRevertError: If k is zero.

    """
    if k == 0:
        raise EVMRevertError(error="division by zero")
    return (x * y) % k


# adapted from OpenZeppelin's overflow checks, which throw
# an exception if the input value exceeds the maximum value
# for this type
def to_int128(x: int) -> int:
    """Convert to int128.

    Returns:
        The input value, guaranteed to fit in int128.

    Raises:
        EVMRevertError: If x is outside int128 range.

    """
    if not (MIN_INT128 <= x <= MAX_INT128):
        raise EVMRevertError(error=f"{x} outside range of int128 values")
    return x


def to_int256(x: int) -> int:
    """Convert to int256.

    Returns:
        The input value, guaranteed to fit in int256.

    Raises:
        EVMRevertError: If x is outside int256 range.

    """
    if not (MIN_INT256 <= x <= MAX_INT256):
        raise EVMRevertError(error=f"{x} outside range of int256 values")
    return x


def to_uint160(x: int) -> int:
    """Convert to uint160.

    Returns:
        The input value, guaranteed to fit in uint160.

    Raises:
        EVMRevertError: If x exceeds uint160 maximum.

    """
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
    """V3 virtual reserves.

    Returns:
        A tuple of (reserve_in, reserve_out) virtual reserves.

    """
    x_virtual = liquidity * Q96 * Q96 // sqrt_price_x96
    y_virtual = liquidity * sqrt_price_x96
    if zero_for_one:
        return x_virtual, y_virtual
    return y_virtual, x_virtual
