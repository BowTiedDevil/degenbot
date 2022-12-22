from . import TickMath
from .Helpers import uint24, MAX_UINT128
from decimal import Decimal

# type hinting aliases
Int24 = int
Uint24 = int


def tickSpacingToMaxLiquidityPerTick(tickSpacing: Int24):

    assert -(2**23) <= tickSpacing <= 2**23 - 1, "input not a valid int24"

    minTick = Decimal(TickMath.MIN_TICK) // tickSpacing * tickSpacing
    maxTick = Decimal(TickMath.MAX_TICK) // tickSpacing * tickSpacing
    numTicks = uint24((maxTick - minTick) // tickSpacing) + 1

    result = MAX_UINT128 // numTicks

    assert 0 <= result <= MAX_UINT128, "result not a valid uint128"

    return result
