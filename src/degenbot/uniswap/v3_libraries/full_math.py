from ...constants import MAX_UINT256, MIN_UINT256
from ...exceptions import EVMRevertError
from .functions import mulmod


def muldiv(
    a: int,
    b: int,
    denominator: int,
) -> int:
    """
    The Solidity implementation is designed to calculate a * b / d without risk of overflowing
    the intermediate result (maximum of 2**256-1).

    Python does not have this bit depth limitations on integers, so simply check for exceptional
    conditions before returning the result.
    """

    if not (MIN_UINT256 <= a <= MAX_UINT256):
        raise EVMRevertError(f"Invalid input, {a} does not fit into uint256")

    if not (MIN_UINT256 <= b <= MAX_UINT256):
        raise EVMRevertError(f"Invalid input, {b} does not fit into uint256")

    if denominator == 0:
        raise EVMRevertError("DIVISION BY ZERO")

    result = (a * b) // denominator

    if not (MIN_UINT256 <= result <= MAX_UINT256):
        raise EVMRevertError("Invalid result, does not fit in uint256")

    return result


def muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    result: int = muldiv(a, b, denominator)
    if mulmod(a, b, denominator) > 0:
        # must be less than max uint256 since we're rounding up
        if not (MIN_UINT256 <= result < MAX_UINT256):
            raise EVMRevertError("FAIL!")
        result += 1
    return result
