from ...exceptions import EVMRevertError
from .functions import uint128


def addDelta(x: int, y: int) -> int:
    """
    This function has been heavily modified to directly check that the result
    fits in a uint128, instead of checking via < or >= tricks via Solidity's
    built-in casting as implemented at https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/LiquidityMath.sol
    """

    if not (0 <= x <= 2**128 - 1):
        raise EVMRevertError("x not a valid uint128")

    if not (-(2**127) <= y <= 2**127 - 1):
        raise EVMRevertError("y not a valid int128")

    if y < 0:
        z = x - uint128(-y)
        if not (0 <= z <= 2**128 - 1):
            raise EVMRevertError("LS")

    else:
        z = x + uint128(y)
        if not (0 <= z <= 2**128 - 1):
            raise EVMRevertError("LA")

    return z
