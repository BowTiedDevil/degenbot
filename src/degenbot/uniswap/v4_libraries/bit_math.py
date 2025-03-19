from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions import EVMRevertError

# This module is adapted from the Uniswap V4 BitMath.sol library.
# Reference: https://github.com/Uniswap/v4-core/blob/main/src/libraries/BitMath.sol


def least_significant_bit(number: int) -> int:
    """
    Find the least significant bit for the given number.
    """

    for index in range(256):
        if (number >> index) & 1 == 1:
            return index

    raise ValueError  # should be unreachable for valid 256 bit numbers

    """
    Find the most significant bit for the given number.
    """

    for index in range(256):
        if (number >> index) == 1:
            return index

    return ValueError  # should be unreachable for valid 256 bit numbers
