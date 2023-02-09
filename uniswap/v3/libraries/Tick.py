from . import TickMath
from .Helpers import *
from decimal import Decimal

# type hinting aliases
Int24 = int
Uint24 = int


def tickSpacingToMaxLiquidityPerTick(tickSpacing: Int24):

    if not (-(2**23) <= tickSpacing <= 2**23 - 1):
        raise EVMRevertError("input not a valid int24")
    # assert -(2**23) <= tickSpacing <= 2**23 - 1, "input not a valid int24"

    minTick = Decimal(TickMath.MIN_TICK) // tickSpacing * tickSpacing
    maxTick = Decimal(TickMath.MAX_TICK) // tickSpacing * tickSpacing
    numTicks = uint24((maxTick - minTick) // tickSpacing) + 1

    result = MAX_UINT128 // numTicks

    if not (0 <= result <= MAX_UINT128):
        raise EVMRevertError("result not a valid uint128")
    # assert 0 <= result <= MAX_UINT128, "result not a valid uint128"

    return result
