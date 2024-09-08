from functools import cache

from ...constants import MAX_UINT256, MIN_UINT256
from ...exceptions import EVMRevertError

# This module is adapted from the Uniswap V3 BitMath.sol library.
# Reference: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/BitMath.sol


@cache
def most_significant_bit(number: int) -> int:
    """
    Find the most significant bit for the given number.

    Adapted from the Uniswap V3 BitMath.sol library.
    Reference: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/BitMath.sol
    """
    if number <= MIN_UINT256:
        raise EVMRevertError("FAIL: x <= 0")
    if number > MAX_UINT256:
        raise EVMRevertError("Number is not a valid uint256")

    msb = 0
    for value, x_shift, msb_shift in (
        (340282366920938463463374607431768211456, 128, 128),
        (18446744073709551616, 64, 64),
        (4294967296, 32, 32),
        (65536, 16, 16),
        (256, 8, 8),
        (16, 4, 4),
        (4, 2, 2),
        (2, 0, 1),
    ):
        if number >= value:
            number >>= x_shift
            msb += msb_shift

    return msb


@cache
def least_significant_bit(number: int) -> int:
    """
    Find the least significant bit for the given number.

    """
    if number <= MIN_UINT256:
        raise EVMRevertError("FAIL: x <= 0")
    if number > MAX_UINT256:
        raise EVMRevertError("Number is not a valid uint256")

    lsb = 255
    for value, x_shift, lsb_shift in (
        (340282366920938463463374607431768211455, 128, 128),
        (18446744073709551615, 64, 64),
        (4294967295, 32, 32),
        (65535, 16, 16),
        (255, 8, 8),
        (15, 4, 4),
        (3, 2, 2),
        (1, 0, 1),
    ):
        if number & value > 0:
            lsb -= lsb_shift
        else:
            number >>= x_shift

    return lsb
