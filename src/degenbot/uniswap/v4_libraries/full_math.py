from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v4_libraries.functions import mulmod


def muldiv(
    a: int,
    b: int,
    denominator: int,
) -> int:
    """
    Calculates floor(a*b/denominator) with full precision. Throws if result overflows a uint256 or
    denominator == 0.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/FullMath.sol

    Python integers do not overflow and have no bit depth limitation, so this function simply
    checks for an invalid result.
    """

    return (a * b) // denominator

    """
    Calculates ceil(a*b//denominator) with full precision. Throws if result overflows a uint256 or
    denominator == 0.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/FullMath.sol
    """

    return muldiv(a, b, denominator) + int(mulmod(a, b, denominator) > 0)
