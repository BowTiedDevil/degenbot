from decimal import Decimal
from math import ceil, floor

from degenbot.constants import MAX_UINT128
from degenbot.uniswap.v3.libraries import Tick

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/Tick.spec.ts

FEE_AMOUNT = {
    "LOW": 500,
    "MEDIUM": 3000,
    "HIGH": 10000,
}

TICK_SPACINGS = {
    FEE_AMOUNT["LOW"]: 10,
    FEE_AMOUNT["MEDIUM"]: 60,
    FEE_AMOUNT["HIGH"]: 200,
}


def getMaxLiquidityPerTick(tick_spacing):
    def getMinTick(tick_spacing):
        return ceil(Decimal(-887272) / tick_spacing) * tick_spacing

    def getMaxTick(tick_spacing):
        return floor(Decimal(887272) / tick_spacing) * tick_spacing

    return round(
        Decimal(2**128 - 1)
        / Decimal(
            1
            + (getMaxTick(tick_spacing) - getMinTick(tick_spacing))
            / tick_spacing
        )
    )


def test_tickSpacingToMaxLiquidityPerTick():
    maxLiquidityPerTick = Tick.tickSpacingToMaxLiquidityPerTick(
        TICK_SPACINGS[FEE_AMOUNT["HIGH"]]
    )
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(
        TICK_SPACINGS[FEE_AMOUNT["HIGH"]]
    )
    assert maxLiquidityPerTick == 38350317471085141830651933667504588

    # returns the correct value for low fee
    maxLiquidityPerTick = Tick.tickSpacingToMaxLiquidityPerTick(
        TICK_SPACINGS[FEE_AMOUNT["LOW"]]
    )
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(
        TICK_SPACINGS[FEE_AMOUNT["LOW"]]
    )
    assert (
        maxLiquidityPerTick == 1917569901783203986719870431555990
    )  # 110.8 bits

    maxLiquidityPerTick = Tick.tickSpacingToMaxLiquidityPerTick(
        TICK_SPACINGS[FEE_AMOUNT["MEDIUM"]]
    )
    assert (
        maxLiquidityPerTick
    ) == 11505743598341114571880798222544994  # 113.1 bits
    assert (maxLiquidityPerTick) == (
        getMaxLiquidityPerTick(TICK_SPACINGS[FEE_AMOUNT["MEDIUM"]])
    )

    # returns the correct value for entire range
    maxLiquidityPerTick = Tick.tickSpacingToMaxLiquidityPerTick(887272)
    assert (maxLiquidityPerTick) == round(
        Decimal(MAX_UINT128) / Decimal(3)
    )  # 126 bits
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(887272)

    # returns the correct value for 2302
    maxLiquidityPerTick = Tick.tickSpacingToMaxLiquidityPerTick(2302)
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(2302)
    assert (
        maxLiquidityPerTick
    ) == 441351967472034323558203122479595605  # 118 bits
