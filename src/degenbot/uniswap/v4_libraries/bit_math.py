# This module is adapted from the Uniswap V4 BitMath.sol library.
# Reference: https://github.com/Uniswap/v4-core/blob/main/src/libraries/BitMath.sol


def least_significant_bit(number: int) -> int:
    """
    Find the least significant bit for the given number.
    """

    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    return (number & -number).bit_length() - 1


def least_significant_bit_legacy(number: int) -> int:
    """
    Find the least significant bit for the given number.
    """

    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    for index in range(256):
        if (number >> index) & 1 == 1:
            return index

    raise ValueError  # should be unreachable for valid 256 bit numbers


def most_significant_bit(number: int) -> int:
    """
    Find the most significant bit for the given number.
    """

    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    return number.bit_length() - 1


def most_significant_bit_legacy(number: int) -> int:
    """
    Find the most significant bit for the given number.
    """

    if number <= 0:
        msg = "Number must be >0"
        raise ValueError(msg)

    for index in range(256):
        if (number >> index) == 1:
            return index

    raise ValueError  # should be unreachable for valid 256 bit numbers
