import hypothesis
import hypothesis.strategies
import pytest
from pydantic import ValidationError

from degenbot.constants import MAX_INT128, MAX_UINT128, MIN_INT128, MIN_UINT128
from degenbot.uniswap.v4_libraries.liquidity_math import add_delta

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/LiquidityMath.t.sol


def test_add_delta_throws_for_underflow():
    with pytest.raises(ValidationError):
        add_delta(0, -1)
    with pytest.raises(ValidationError):
        add_delta(MAX_INT128, MIN_INT128)


def test_add_delta_throws_for_overflow():
    with pytest.raises(ValidationError):
        add_delta(MAX_UINT128, 1)


@hypothesis.given(
    x=hypothesis.strategies.integers(min_value=-MIN_INT128, max_value=MAX_UINT128),
)
def test_add_delta_sub_int128min_fuzz(x: int):
    assert add_delta(x, MIN_INT128) == x - (-MIN_INT128)


@hypothesis.given(
    x=hypothesis.strategies.integers(min_value=MIN_UINT128, max_value=MAX_UINT128),
    y=hypothesis.strategies.integers(min_value=MIN_INT128, max_value=MAX_INT128),
)
def test_add_delta_fuzz(x: int, y: int):
    hypothesis.assume(y != MIN_INT128)

    try:
        add_delta(x, y)
    except ValidationError:
        assert x + y < MIN_UINT128 or x + y > MAX_UINT128
