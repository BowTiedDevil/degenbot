from decimal import Decimal, getcontext
from math import ceil, floor

from degenbot.constants import MAX_UINT128
from degenbot.uniswap.v3_libraries.tick import tick_spacing_to_max_liquidity_per_tick

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

# Change the rounding method to match the BigNumber unit test at https://github.com/Uniswap/v3-core/blob/main/test/shared/utilities.ts
# which specifies .integerValue(3), the 'ROUND_FLOOR' rounding method per https://mikemcl.github.io/bignumber.js/#bignumber
getcontext().prec = 256
getcontext().rounding = "ROUND_FLOOR"


def getMaxLiquidityPerTick(tick_spacing: int) -> int:
    def getMinTick(tick_spacing: int) -> int:
        return ceil(Decimal(-887272) / tick_spacing) * tick_spacing

    def getMaxTick(tick_spacing: int) -> int:
        return floor(Decimal(887272) / tick_spacing) * tick_spacing

    return round(
        (2**128 - 1) // (1 + (getMaxTick(tick_spacing) - getMinTick(tick_spacing)) // tick_spacing)
    )


def test_tickSpacingToMaxLiquidityPerTick():
    maxLiquidityPerTick = tick_spacing_to_max_liquidity_per_tick(TICK_SPACINGS[FEE_AMOUNT["HIGH"]])
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(TICK_SPACINGS[FEE_AMOUNT["HIGH"]])
    assert maxLiquidityPerTick == 38350317471085141830651933667504588

    # returns the correct value for low fee
    maxLiquidityPerTick = tick_spacing_to_max_liquidity_per_tick(TICK_SPACINGS[FEE_AMOUNT["LOW"]])
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(TICK_SPACINGS[FEE_AMOUNT["LOW"]])
    assert maxLiquidityPerTick == 1917569901783203986719870431555990  # 110.8 bits

    maxLiquidityPerTick = tick_spacing_to_max_liquidity_per_tick(
        TICK_SPACINGS[FEE_AMOUNT["MEDIUM"]]
    )
    assert (maxLiquidityPerTick) == 11505743598341114571880798222544994  # 113.1 bits
    assert (maxLiquidityPerTick) == (getMaxLiquidityPerTick(TICK_SPACINGS[FEE_AMOUNT["MEDIUM"]]))

    # returns the correct value for entire range
    maxLiquidityPerTick = tick_spacing_to_max_liquidity_per_tick(887272)
    assert (maxLiquidityPerTick) == round(Decimal(MAX_UINT128) / Decimal(3))  # 126 bits
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(887272)

    # returns the correct value for 2302
    maxLiquidityPerTick = tick_spacing_to_max_liquidity_per_tick(2302)
    assert maxLiquidityPerTick == getMaxLiquidityPerTick(2302)
    assert (maxLiquidityPerTick) == 441351967472034323558203122479595605  # 118 bits
