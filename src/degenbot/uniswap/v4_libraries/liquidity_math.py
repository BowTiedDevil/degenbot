from degenbot.constants import MAX_UINT128, MIN_UINT128
from degenbot.exceptions import EVMRevertError


def add_delta(x: int, y: int) -> int:
    """
    This function has been modified to directly check that the result fits in a uint128, instead
    of inline Yul as implemented at
    https://github.com/Uniswap/v4-core/blob/main/src/libraries/LiquidityMath.sol
    """

    z = x + y

    if not (MIN_UINT128 <= z <= MAX_UINT128):
        raise EVMRevertError(error="SafeCastOverflow")

    return z
