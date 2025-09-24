from degenbot.constants import MAX_UINT128, MIN_UINT128
from degenbot.exceptions.evm import EVMRevertError


def add_delta(
    x: int,
    y: int,
) -> int:
    """
    This function has been modified to check that the result fits in a uint128, instead
    of inline Yul as implemented by the Solidity contract.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/LiquidityMath.sol
    """

    result = x + y
    if result < MIN_UINT128 or result > MAX_UINT128:
        msg = "SafeCastOverflow"
        raise EVMRevertError(msg)

    return result
