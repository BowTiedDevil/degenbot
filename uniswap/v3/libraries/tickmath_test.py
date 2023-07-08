from decimal import Decimal, localcontext
from math import floor, log

import pytest

from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import TickMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickMath.spec.ts


def encodePriceSqrt(reserve1: int, reserve0: int):
    """
    Returns the sqrt price as a Q64.96 value
    """
    with localcontext() as ctx:
        # Change the rounding method to match the BigNumber unit test at https://github.com/Uniswap/v3-core/blob/main/test/shared/utilities.ts
        # which specifies .integerValue(3), the 'ROUND_FLOOR' rounding method per https://mikemcl.github.io/bignumber.js/#bignumber
        ctx.rounding = "ROUND_FLOOR"
        return round(
            (Decimal(reserve1) / Decimal(reserve0)).sqrt() * Decimal(2**96)
        )


def test_getSqrtRatioAtTick():
    with pytest.raises(EVMRevertError, match="T"):
        TickMath.getSqrtRatioAtTick(TickMath.MIN_TICK - 1)

    with pytest.raises(EVMRevertError, match="T"):
        TickMath.getSqrtRatioAtTick(TickMath.MAX_TICK + 1)

    assert TickMath.getSqrtRatioAtTick(TickMath.MIN_TICK) == 4295128739

    assert TickMath.getSqrtRatioAtTick(TickMath.MIN_TICK + 1) == 4295343490

    assert (
        TickMath.getSqrtRatioAtTick(TickMath.MAX_TICK - 1)
        == 1461373636630004318706518188784493106690254656249
    )

    assert TickMath.getSqrtRatioAtTick(TickMath.MIN_TICK) < (
        encodePriceSqrt(1, 2**127)
    )

    assert TickMath.getSqrtRatioAtTick(TickMath.MAX_TICK) > encodePriceSqrt(
        2**127, 1
    )

    assert (
        TickMath.getSqrtRatioAtTick(TickMath.MAX_TICK)
        == 1461446703485210103287273052203988822378723970342
    )


def test_minSqrtRatio():
    min = TickMath.getSqrtRatioAtTick(TickMath.MIN_TICK)
    assert min == TickMath.MIN_SQRT_RATIO


def test_maxSqrtRatio():
    max = TickMath.getSqrtRatioAtTick(TickMath.MAX_TICK)
    assert max == TickMath.MAX_SQRT_RATIO


def test_getTickAtSqrtRatio():
    with pytest.raises(EVMRevertError, match="R"):
        TickMath.getTickAtSqrtRatio(TickMath.MIN_SQRT_RATIO - 1)

    with pytest.raises(EVMRevertError, match="R"):
        TickMath.getTickAtSqrtRatio(TickMath.MAX_SQRT_RATIO)

    assert (TickMath.getTickAtSqrtRatio(TickMath.MIN_SQRT_RATIO)) == (
        TickMath.MIN_TICK
    )
    assert (TickMath.getTickAtSqrtRatio(4295343490)) == (TickMath.MIN_TICK + 1)

    assert (
        TickMath.getTickAtSqrtRatio(
            1461373636630004318706518188784493106690254656249
        )
    ) == (TickMath.MAX_TICK - 1)
    assert (
        TickMath.getTickAtSqrtRatio(TickMath.MAX_SQRT_RATIO - 1)
    ) == TickMath.MAX_TICK - 1

    for ratio in [
        TickMath.MIN_SQRT_RATIO,
        encodePriceSqrt((10) ** (12), 1),
        encodePriceSqrt((10) ** (6), 1),
        encodePriceSqrt(1, 64),
        encodePriceSqrt(1, 8),
        encodePriceSqrt(1, 2),
        encodePriceSqrt(1, 1),
        encodePriceSqrt(2, 1),
        encodePriceSqrt(8, 1),
        encodePriceSqrt(64, 1),
        encodePriceSqrt(1, (10) ** (6)),
        encodePriceSqrt(1, (10) ** (12)),
        TickMath.MAX_SQRT_RATIO - 1,
    ]:
        math_result = floor(log(((ratio / 2**96) ** 2), 1.0001))
        result = TickMath.getTickAtSqrtRatio(ratio)
        abs_diff = abs(result - math_result)
        assert abs_diff <= 1

        tick = TickMath.getTickAtSqrtRatio(ratio)
        ratio_of_tick = TickMath.getSqrtRatioAtTick(tick)
        ratio_of_tick_plus_one = TickMath.getSqrtRatioAtTick(tick + 1)
        assert ratio >= ratio_of_tick
        assert ratio < ratio_of_tick_plus_one
