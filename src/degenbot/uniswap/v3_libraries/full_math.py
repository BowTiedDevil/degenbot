"""Uniswap V3 FullMath: 512-bit multiplication with Q96 rounding."""
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions.pool import EVMRevertError
from degenbot.uniswap.v3_libraries.functions import mulmod


def muldiv(
    a: int,
    b: int,
    denominator: int,
) -> int:
    """
    Compute a * b / d with full 512-bit precision, matching the Solidity implementation designed to.

    the intermediate result.

    Python integers do not overflow and have no bit depth limitation, so this function simply
    checks for an invalid result.

    ref: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/FullMath.sol
    """
    # Assert values are valid for Solidity contract
    if a < MIN_UINT256 or a > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for a.")
    if b < MIN_UINT256 or b > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for b.")
    if denominator < MIN_UINT256 or denominator > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for denominator.")

    if denominator == 0:
        raise EVMRevertError(error="DIVISION BY ZERO")

    result = (a * b) // denominator

    if not (MIN_UINT256 <= result <= MAX_UINT256):
        raise EVMRevertError(error="Invalid result, does not fit in uint256")

    return result


def muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    """Return muldiv rounding up."""
    result = muldiv(a, b, denominator)
    if mulmod(a, b, denominator) > 0:
        # must be less than max uint256 since we're rounding up
        if not (MIN_UINT256 <= result < MAX_UINT256):
            raise EVMRevertError(error="FAIL!")
        return result + 1
    return result
