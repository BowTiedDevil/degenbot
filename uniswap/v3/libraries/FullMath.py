from . import YulOperations as yul
from .Helpers import *

# type hinting aliases
Uint256 = int


class FullMathException(Exception):
    pass


def mulDiv(a: Uint256, b: Uint256, denominator: Uint256):
    """
    The Solidity implementation is designed to calculate a * b / d without risk of overflowing
    the intermediate result (maximum of 2**256-1).

    Python does not have this bit depth limitations on integers,
    so simply check for exceptional conditions then return the result
    """

    assert MIN_UINT256 <= a <= MAX_UINT256, FullMathException(
        f"Invalid input, {a} does not fit into uint256"
    )
    assert MIN_UINT256 <= b <= MAX_UINT256, FullMathException(
        f"Invalid input, {b} does not fit into uint256"
    )
    assert denominator != 0, FullMathException("DIVISION BY ZERO")

    result: Uint256 = (a * b) // denominator

    assert MIN_UINT256 <= result <= MAX_UINT256, FullMathException(
        "invalid result, will not fit in uint256"
    )

    return result


def mulDivRoundingUp(a: Uint256, b: Uint256, denominator: Uint256):
    result: Uint256 = mulDiv(a, b, denominator)
    if mulmod(a, b, denominator) > 0:
        # must be less than max uint256 since we're rounding up
        assert MIN_UINT256 <= result < MAX_UINT256, "FAIL!"
        result += 1
    return result
