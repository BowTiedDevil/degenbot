from degenbot.constants import MAX_INT128, MAX_UINT128, MIN_INT128, MIN_UINT128
from degenbot.exceptions import EVMRevertError


def add_delta(x: int, y: int) -> int:
    """
    This function has been heavily modified to directly check that the result
    fits in a uint128, instead of checking via < or >= tricks via Solidity's
    built-in casting as implemented at https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/LiquidityMath.sol
    """

    if not (MIN_UINT128 <= x <= MAX_UINT128):
        raise EVMRevertError(error="x not a valid uint128")
    if not (MIN_INT128 <= y <= MAX_INT128):
        raise EVMRevertError(error="y not a valid int128")

    z = x + y

    if y < 0 and not (MIN_UINT128 <= z <= MAX_UINT128):
        raise EVMRevertError(error="LS")
    if not (MIN_UINT128 <= z <= MAX_UINT128):
        raise EVMRevertError(error="LA")

    return z
