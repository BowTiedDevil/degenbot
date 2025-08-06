from pydantic import validate_call

from degenbot.validation.evm_values import ValidatedInt128, ValidatedUint128


@validate_call(validate_return=True)
def add_delta(
    x: ValidatedUint128,
    y: ValidatedInt128,
) -> ValidatedUint128:
    """
    This function has been modified to check that the result fits in a uint128, instead
    of inline Yul as implemented by the Solidity contract.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/LiquidityMath.sol
    """

    return x + y
