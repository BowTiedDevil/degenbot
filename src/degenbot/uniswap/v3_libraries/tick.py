from decimal import Decimal

from ...constants import MAX_UINT128
from . import tick_math as TickMath


def tickSpacingToMaxLiquidityPerTick(tickSpacing: int) -> int:
    minTick = Decimal(TickMath.MIN_TICK) // tickSpacing * tickSpacing
    maxTick = Decimal(TickMath.MAX_TICK) // tickSpacing * tickSpacing
    numTicks = ((maxTick - minTick) // tickSpacing) + 1
    return round(MAX_UINT128 // numTicks)
