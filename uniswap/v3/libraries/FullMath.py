from degenbot.exceptions import EVMRevertError
from .Helpers import *

# type hinting aliases
Uint256 = int


def mulDiv(a: Uint256, b: Uint256, denominator: Uint256):
    """
    The Solidity implementation is designed to calculate a * b / d without risk of overflowing
    the intermediate result (maximum of 2**256-1).

    Python does not have this bit depth limitations on integers,
    so simply check for exceptional conditions then return the result
    """

    if not (MIN_UINT256 <= a <= MAX_UINT256):
        raise EVMRevertError(f"Invalid input, {a} does not fit into uint256")

    if not (MIN_UINT256 <= b <= MAX_UINT256):
        raise EVMRevertError(f"Invalid input, {b} does not fit into uint256")

    if denominator == 0:
        raise EVMRevertError("DIVISION BY ZERO")

    result: Uint256 = (a * b) // denominator

    if not (MIN_UINT256 <= result <= MAX_UINT256):
        raise EVMRevertError("invalid result, will not fit in uint256")

    return result


def mulDivRoundingUp(a: Uint256, b: Uint256, denominator: Uint256):
    result: Uint256 = mulDiv(a, b, denominator)
    if mulmod(a, b, denominator) > 0:
        # must be less than max uint256 since we're rounding up
        if not (MIN_UINT256 <= result < MAX_UINT256):
            raise EVMRevertError("FAIL!")
        result += 1
    return result
