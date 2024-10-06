from ...constants import MAX_UINT256, MIN_UINT256
from ...exceptions import EVMRevertError

# This module is adapted from the Uniswap V3 BitMath.sol library.
# Reference: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/BitMath.sol


def least_significant_bit(number: int):
    """
    Find the least significant bit for the given number.

    This function is rewritten to use simple string manipulation instead of the binary search
    implemented by the official Solidity contract.
    """

    if number <= MIN_UINT256:
        raise EVMRevertError("FAIL: x <= 0")
    if number > MAX_UINT256:
        raise EVMRevertError("Number is not a valid uint256")

    # Reverse the binary string and search for LSB by returning the position of the first "1" value
    #
    # e.g. bin(69) == '0b1000101'
    # trim the '0b' and reverse the string to '1010001'
    # LSB occurs at position 0
    num_string = bin(number)[2:][::-1]
    return num_string.find("1")


def most_significant_bit(number: int) -> int:
    """
    Find the most significant bit for the given number.

    This function is rewritten to use simple string manipulation instead of the binary search
    implemented by the official Solidity contract.
    """
    if number <= MIN_UINT256:
        raise EVMRevertError("FAIL: x <= 0")
    if number > MAX_UINT256:
        raise EVMRevertError("Number is not a valid uint256")

    # Reverse the binary string and search for LSB by returning the position of the first "1" value
    #
    # e.g. bin(69) == '0b1000101', trim the '0b'
    # LSB occurs at position 6
    num_string = bin(number)[2:]
    return (len(num_string) - 1) - num_string.find("1")
