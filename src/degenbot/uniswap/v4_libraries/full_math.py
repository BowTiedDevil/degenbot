from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError
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

    if denominator <= 0:
        msg = "required: denominator > 0"
        raise EVMRevertError(msg)

    result = (a * b) // denominator

    if result > MAX_UINT256:
        msg = "product > MAX_UINT256"
        raise EVMRevertError(msg)

    return result


def muldiv_rounding_up(
    a: int,
    b: int,
    denominator: int,
) -> int:
    """
    Calculates ceil(a*b//denominator) with full precision. Throws if result overflows a uint256 or
    denominator == 0.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/FullMath.sol
    """

    if denominator <= 0:
        msg = "required: denominator > 0"
        raise EVMRevertError(msg)

    result = muldiv(a, b, denominator) + int(mulmod(a, b, denominator) > 0)

    if result > MAX_UINT256:
        msg = "product > MAX_UINT256"
        raise EVMRevertError(msg)

    return result
