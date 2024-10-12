from decimal import Decimal, getcontext
from math import floor, log

import pytest

from degenbot.constants import MAX_UINT160, MIN_UINT160
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickMath.spec.ts

# Change the rounding method to match the BigNumber unit test at https://github.com/Uniswap/v3-core/blob/main/test/shared/utilities.ts
# which specifies .integerValue(3), the 'ROUND_FLOOR' rounding method per https://mikemcl.github.io/bignumber.js/#bignumber
getcontext().prec = 256
getcontext().rounding = "ROUND_FLOOR"


def encodePriceSqrt(reserve1: int, reserve0: int) -> int:
    """
    Returns the sqrt price as a Q64.96 value
    """
    return round((Decimal(reserve1) / Decimal(reserve0)).sqrt() * Decimal(2**96))


def test_getSqrtRatioAtTick() -> None:
    with pytest.raises(EVMRevertError, match="T"):
        get_sqrt_ratio_at_tick(MIN_TICK - 1)

    with pytest.raises(EVMRevertError, match="T"):
        get_sqrt_ratio_at_tick(MAX_TICK + 1)

    assert get_sqrt_ratio_at_tick(MIN_TICK) == 4295128739

    assert get_sqrt_ratio_at_tick(MIN_TICK + 1) == 4295343490

    assert get_sqrt_ratio_at_tick(MAX_TICK - 1) == 1461373636630004318706518188784493106690254656249

    assert get_sqrt_ratio_at_tick(MIN_TICK) < (encodePriceSqrt(1, 2**127))

    assert get_sqrt_ratio_at_tick(MAX_TICK) > encodePriceSqrt(2**127, 1)

    assert get_sqrt_ratio_at_tick(MAX_TICK) == 1461446703485210103287273052203988822378723970342


def test_minSqrtRatio() -> None:
    min = get_sqrt_ratio_at_tick(MIN_TICK)
    assert min == MIN_SQRT_RATIO


def test_maxSqrtRatio() -> None:
    max = get_sqrt_ratio_at_tick(MAX_TICK)
    assert max == MAX_SQRT_RATIO


def test_getTickAtSqrtRatio() -> None:
    with pytest.raises(EVMRevertError, match="Not a valid uint160"):
        get_tick_at_sqrt_ratio(MIN_UINT160 - 1)

    with pytest.raises(EVMRevertError, match="Not a valid uint160"):
        get_tick_at_sqrt_ratio(MAX_UINT160 + 1)

    with pytest.raises(EVMRevertError, match="R"):
        get_tick_at_sqrt_ratio(MIN_SQRT_RATIO - 1)

    with pytest.raises(EVMRevertError, match="R"):
        get_tick_at_sqrt_ratio(MAX_SQRT_RATIO)

    assert (get_tick_at_sqrt_ratio(MIN_SQRT_RATIO)) == (MIN_TICK)
    assert (get_tick_at_sqrt_ratio(4295343490)) == (MIN_TICK + 1)

    assert (get_tick_at_sqrt_ratio(1461373636630004318706518188784493106690254656249)) == (
        MAX_TICK - 1
    )
    assert (get_tick_at_sqrt_ratio(MAX_SQRT_RATIO - 1)) == MAX_TICK - 1

    for ratio in [
        MIN_SQRT_RATIO,
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
        MAX_SQRT_RATIO - 1,
    ]:
        math_result = floor(log(((ratio / 2**96) ** 2), 1.0001))
        result = get_tick_at_sqrt_ratio(ratio)
        abs_diff = abs(result - math_result)
        assert abs_diff <= 1

        tick = get_tick_at_sqrt_ratio(ratio)
        ratio_of_tick = get_sqrt_ratio_at_tick(tick)
        ratio_of_tick_plus_one = get_sqrt_ratio_at_tick(tick + 1)
        assert ratio >= ratio_of_tick
        assert ratio < ratio_of_tick_plus_one
