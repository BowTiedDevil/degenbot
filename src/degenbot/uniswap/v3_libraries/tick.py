from decimal import Decimal

from ...constants import MAX_UINT128
from ...exceptions import EVMRevertError
from . import tick_math as TickMath

# type hinting aliases
Int24 = int


def tickSpacingToMaxLiquidityPerTick(tickSpacing: Int24):
    if not (-(2**23) <= tickSpacing <= 2**23 - 1):
        raise EVMRevertError("input not a valid int24")

    minTick = Decimal(TickMath.MIN_TICK) // tickSpacing * tickSpacing
    maxTick = Decimal(TickMath.MAX_TICK) // tickSpacing * tickSpacing
    numTicks = ((maxTick - minTick) // tickSpacing) + 1

    result = MAX_UINT128 // numTicks

    if not (0 <= result <= MAX_UINT128):
        raise EVMRevertError("result not a valid uint128")

    return result
