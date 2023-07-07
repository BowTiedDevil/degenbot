from decimal import Decimal

from degenbot.constants import MAX_UINT128
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import TickMath
from degenbot.uniswap.v3.libraries.functions import uint24

# type hinting aliases
Int24 = int


def tickSpacingToMaxLiquidityPerTick(tickSpacing: Int24):
    if not (-(2**23) <= tickSpacing <= 2**23 - 1):
        raise EVMRevertError("input not a valid int24")

    minTick = Decimal(TickMath.MIN_TICK) // tickSpacing * tickSpacing
    maxTick = Decimal(TickMath.MAX_TICK) // tickSpacing * tickSpacing
    numTicks = uint24((maxTick - minTick) // tickSpacing) + 1

    result = MAX_UINT128 // numTicks

    if not (0 <= result <= MAX_UINT128):
        raise EVMRevertError("result not a valid uint128")

    return result
