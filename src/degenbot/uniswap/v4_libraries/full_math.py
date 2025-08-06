from pydantic import validate_call

from degenbot.uniswap.v4_libraries.functions import mulmod
from degenbot.validation.evm_values import ValidatedUint256, ValidatedUint256NonZero


@validate_call(validate_return=True)
def muldiv(
    a: ValidatedUint256,
    b: ValidatedUint256,
    denominator: ValidatedUint256NonZero,
) -> ValidatedUint256:
    """
    Calculates floor(a*b/denominator) with full precision. Throws if result overflows a uint256 or
    denominator == 0.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/FullMath.sol

    Python integers do not overflow and have no bit depth limitation, so this function simply
    checks for an invalid result.
    """

    return (a * b) // denominator


@validate_call(validate_return=True)
def muldiv_rounding_up(
    a: ValidatedUint256,
    b: ValidatedUint256,
    denominator: ValidatedUint256NonZero,
) -> ValidatedUint256:
    """
    Calculates ceil(a*b//denominator) with full precision. Throws if result overflows a uint256 or
    denominator == 0.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/FullMath.sol
    """

    return muldiv(a, b, denominator) + int(mulmod(a, b, denominator) > 0)
