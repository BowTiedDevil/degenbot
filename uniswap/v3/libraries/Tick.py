from . import TickMath
from .Helpers import uint24

MAXUINT128 = 2**128 - 1


def tickSpacingToMaxLiquidityPerTick(tickSpacing: int):
    minTick: int = (TickMath.MIN_TICK // tickSpacing) * tickSpacing
    maxTick: int = (TickMath.MAX_TICK // tickSpacing) * tickSpacing
    numTicks: int = uint24((maxTick - minTick) // tickSpacing) + 1
    return MAXUINT128 // numTicks
