from pydantic import validate_call

from degenbot.validation.evm_values import ValidatedUint8, ValidatedUint256NonZero

# This module is adapted from the Uniswap V4 BitMath.sol library.
# Reference: https://github.com/Uniswap/v4-core/blob/main/src/libraries/BitMath.sol


@validate_call(validate_return=True)
def least_significant_bit(number: ValidatedUint256NonZero) -> ValidatedUint8:
    """
    Find the least significant bit for the given number.
    """

    for index in range(256):
        if (number >> index) & 1 == 1:
            return index

    raise ValueError  # should be unreachable for valid 256 bit numbers


@validate_call(validate_return=True)
def most_significant_bit(number: ValidatedUint256NonZero) -> ValidatedUint8:
    """
    Find the most significant bit for the given number.
    """

    for index in range(256):
        if (number >> index) == 1:
            return index

    raise ValueError  # should be unreachable for valid 256 bit numbers
