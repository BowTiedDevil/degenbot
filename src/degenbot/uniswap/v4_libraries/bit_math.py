# This module is adapted from the Uniswap V4 BitMath.sol library.
"""Uniswap V4 BitMath: bit position and population count."""
# Reference: https://github.com/Uniswap/v4-core/blob/main/src/libraries/BitMath.sol


def least_significant_bit(number: int) -> int:
    """Find the least significant bit for the given number.

    Returns:
        The position of the least significant bit.

    Raises:
        ValueError: If number is not positive.

    """
    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    return (number & -number).bit_length() - 1


def most_significant_bit(number: int) -> int:
    """Find the most significant bit for the given number.

    Returns:
        The position of the most significant bit.

    Raises:
        ValueError: If number is not positive.

    """
    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    return number.bit_length() - 1
