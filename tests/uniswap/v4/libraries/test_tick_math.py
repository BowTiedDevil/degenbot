import hypothesis
import hypothesis.strategies
import pytest

from degenbot.constants import MAX_INT24, MAX_UINT160, MIN_INT24, MIN_UINT160
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.v4_libraries import tick_math

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/TickMath.t.sol


def test_min_tick_equals_negative_max_tick():
    assert tick_math.MIN_TICK == tick_math.MAX_TICK * -1


def test_max_tick_equals_negative_min_tick():
    assert tick_math.MAX_TICK == tick_math.MIN_TICK * -1


def test_get_sqrt_price_at_tick_throws_for_int24_min():
    tick = MIN_INT24
    with pytest.raises(EVMRevertError, match="InvalidTick"):
        tick_math.get_sqrt_price_at_tick(tick)


def test_get_sqrt_price_at_tick_throws_for_too_low():
    tick = tick_math.MIN_TICK - 1
    with pytest.raises(EVMRevertError, match="InvalidTick"):
        tick_math.get_sqrt_price_at_tick(tick)


def test_get_sqrt_price_at_tick_throws_for_too_high():
    tick = tick_math.MAX_TICK + 1
    with pytest.raises(EVMRevertError, match="InvalidTick"):
        tick_math.get_sqrt_price_at_tick(tick)


@hypothesis.given(
    tick=hypothesis.strategies.integers(
        min_value=MIN_INT24,
        max_value=MAX_INT24,
    )
)
def test_fuzz_get_sqrt_price_at_tick_throws_for_too_large(tick: int):
    if tick > 0:
        hypothesis.assume(tick >= tick_math.MAX_TICK + 1)
    else:
        hypothesis.assume(tick <= tick_math.MIN_TICK - 1)

    with pytest.raises(EVMRevertError, match="InvalidTick"):
        tick_math.get_sqrt_price_at_tick(tick)


def test_get_sqrt_price_at_tick_is_valid_min_tick():
    assert tick_math.get_sqrt_price_at_tick(tick_math.MIN_TICK) == tick_math.MIN_SQRT_PRICE
    assert tick_math.get_sqrt_price_at_tick(tick_math.MIN_TICK) == 4295128739


def test_get_sqrt_price_at_tick_is_valid_min_tick_add_one():
    assert tick_math.get_sqrt_price_at_tick(tick_math.MIN_TICK + 1) == 4295343490


def test_get_sqrt_price_at_tick_is_valid_max_tick():
    assert tick_math.get_sqrt_price_at_tick(tick_math.MAX_TICK) == tick_math.MAX_SQRT_PRICE
    assert (
        tick_math.get_sqrt_price_at_tick(tick_math.MAX_TICK)
        == 1461446703485210103287273052203988822378723970342
    )


def test_get_sqrt_price_at_tick_is_valid_max_tick_sub_one():
    assert (
        tick_math.get_sqrt_price_at_tick(tick_math.MAX_TICK - 1)
        == 1461373636630004318706518188784493106690254656249
    )


def test_get_sqrt_price_at_tick_is_less_than_js_impl_min_tick():
    # sqrt(1 / 2 ** 127) * 2 ** 96
    js_min_sqrt_price = 6085630636
    sol_min_sqrt_price = tick_math.get_sqrt_price_at_tick(tick_math.MIN_TICK)
    assert sol_min_sqrt_price < js_min_sqrt_price


def test_get_sqrt_price_at_tick_is_greater_than_js_impl_max_tick():
    # sqrt(2 ** 127) * 2 ** 96
    js_max_sqrt_price = 1033437718471923706666374484006904511252097097914
    sol_max_sqrt_price = tick_math.get_sqrt_price_at_tick(tick_math.MAX_TICK)
    assert sol_max_sqrt_price > js_max_sqrt_price


def test_get_tick_at_sqrt_price_throws_for_too_low():
    with pytest.raises(EVMRevertError, match="InvalidSqrtPrice"):
        tick_math.get_tick_at_sqrt_price(tick_math.MIN_SQRT_PRICE - 1)


def test_get_tick_at_sqrt_price_throws_for_too_high():
    with pytest.raises(EVMRevertError, match="InvalidSqrtPrice"):
        tick_math.get_tick_at_sqrt_price(tick_math.MAX_SQRT_PRICE + 1)


@hypothesis.given(
    sqrt_price_x96=hypothesis.strategies.integers(
        min_value=MIN_UINT160,
        max_value=tick_math.MIN_SQRT_PRICE - 1,
    )
)
def test_fuzz_get_tick_at_sqrt_price_throws_for_price_too_low(sqrt_price_x96: int):
    with pytest.raises(EVMRevertError, match="InvalidSqrtPrice"):
        tick_math.get_tick_at_sqrt_price(sqrt_price_x96)


@hypothesis.given(
    sqrt_price_x96=hypothesis.strategies.integers(
        min_value=tick_math.MAX_SQRT_PRICE + 1,
        max_value=MAX_UINT160,
    )
)
def test_fuzz_get_tick_at_sqrt_price_throws_for_price_too_high(sqrt_price_x96: int):
    with pytest.raises(EVMRevertError, match="InvalidSqrtPrice"):
        tick_math.get_tick_at_sqrt_price(sqrt_price_x96)


def test_get_tick_at_sqrt_price_is_valid_min_sqrt_price():
    assert tick_math.get_tick_at_sqrt_price(tick_math.MIN_SQRT_PRICE) == tick_math.MIN_TICK


def test_get_tick_at_sqrt_price_is_valid_min_sqrt_price_plus_one():
    assert tick_math.get_tick_at_sqrt_price(4295343490) == tick_math.MIN_TICK + 1


def test_get_tick_at_sqrt_price_is_valid_price_closest_to_max_tick():
    assert tick_math.get_tick_at_sqrt_price(tick_math.MAX_SQRT_PRICE - 1) == tick_math.MAX_TICK - 1


def test_get_tick_at_sqrt_price_is_valid_max_sqrt_price_minus_one():
    assert (
        tick_math.get_tick_at_sqrt_price(1461373636630004318706518188784493106690254656249)
        == tick_math.MAX_TICK - 1
    )


# skipped JavaScript tests


@hypothesis.given(
    tick=hypothesis.strategies.integers(
        min_value=tick_math.MIN_TICK,
        max_value=tick_math.MAX_TICK - 1,
    )
)
def test_fuzz_get_tick_at_sqrt_price_get_sqrt_price_at_tick_relation(tick: int):
    next_tick = tick + 1
    price_at_tick = tick_math.get_sqrt_price_at_tick(tick)
    price_at_next_tick = tick_math.get_sqrt_price_at_tick(next_tick)
    # check lowest price of tick
    assert tick_math.get_tick_at_sqrt_price(price_at_tick) == tick, "lower price"
    # check mid price of tick
    assert tick_math.get_tick_at_sqrt_price((price_at_tick + price_at_next_tick) // 2) == tick, (
        "mid price"
    )
    # check upper price of tick
    assert tick_math.get_tick_at_sqrt_price(price_at_next_tick - 1) == tick, "upper price"
    # check lower price of next tick
    assert tick_math.get_tick_at_sqrt_price(price_at_next_tick) == next_tick, (
        "lower price next tick"
    )


# skipped gas tests
