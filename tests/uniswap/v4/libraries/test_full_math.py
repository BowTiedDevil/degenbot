import hypothesis
import hypothesis.strategies
import pytest
from pydantic import ValidationError

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.uniswap.v4_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v4_libraries.functions import mulmod

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/FullMath.t.sol


Q128 = 2**128


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_fuzz_mul_div_reverts_with0_denominator(x: int, y: int):
    with pytest.raises(ValidationError):
        muldiv(x, y, 0)


def test_mul_div_reverts_with_overflowing_numerator_and_zero_denominator():
    with pytest.raises(ValidationError):
        muldiv(Q128, Q128, 0)


def test_mul_div_reverts_if_output_overflows():
    with pytest.raises(ValidationError):
        muldiv(Q128, Q128, 1)


def test_mul_div_reverts_overflow_with_all_max_inputs():
    with pytest.raises(ValidationError):
        muldiv(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)


def test_mul_div_valid_all_max_inputs():
    assert muldiv(MAX_UINT256, MAX_UINT256, MAX_UINT256) == MAX_UINT256


def test_mul_div_valid_without_phantom_overflow():
    result = Q128 // 3
    assert muldiv(Q128, 50 * Q128 // 100, 150 * Q128 // 100) == result


def test_mul_div_valid_with_phantom_overflow():
    result = 4375 * Q128 // 1000
    assert muldiv(Q128, 35 * Q128, 8 * Q128) == result


def test_mul_div_phantom_overflow_repeating_decimal():
    result = 1 * Q128 // 3
    assert muldiv(Q128, 1000 * Q128, 3000 * Q128) == result


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    d=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_fuzz_mul_div(x: int, y: int, d: int):
    hypothesis.assume(d != 0)
    hypothesis.assume(y != 0)
    hypothesis.assume(0 <= x <= MAX_UINT256 // y)

    assert muldiv(x, y, d) == x * y // d


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_fuzz_mul_div_rounding_up_reverts_with0_denominator(x: int, y: int):
    with pytest.raises(ValidationError):
        muldiv_rounding_up(x, y, 0)


def test_mul_div_rounding_up_valid_with_all_max_inputs():
    assert muldiv_rounding_up(MAX_UINT256, MAX_UINT256, MAX_UINT256) == MAX_UINT256


def test_mul_div_rounding_up_valid_with_no_phantom_overflow():
    result = Q128 // 3 + 1
    assert muldiv_rounding_up(Q128, 50 * Q128 // 100, 150 * Q128 // 100) == result


def test_mul_div_rounding_up_valid_with_phantom_overflow():
    # NOTE: test on github uses muldiv incorrectly, should be mulDivRoundingUp
    # TODO: file bug
    result = 4375 * Q128 // 1000
    assert muldiv_rounding_up(Q128, 35 * Q128, 8 * Q128) == result


def test_mul_div_rounding_up_valid_with_phantom_overflow_repeating_decimal():
    result = 1 * Q128 // 3 + 1
    assert muldiv_rounding_up(Q128, 1000 * Q128, 3000 * Q128) == result


def test_mul_div_rounding_up_reverts_if_mul_div_overflows256_bits_after_rounding_up():
    with pytest.raises(ValidationError):
        muldiv_rounding_up(
            535006138814359,
            432862656469423142931042426214547535783388063929571229938474969,
            2,
        )


def test_mul_div_rounding_up_reverts_if_mul_div_overflows256_bits_after_rounding_up_case2():
    with pytest.raises(ValidationError):
        muldiv_rounding_up(
            115792089237316195423570985008687907853269984659341747863450311749907997002549,
            115792089237316195423570985008687907853269984659341747863450311749907997002550,
            115792089237316195423570985008687907853269984653042931687443039491902864365164,
        )


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    d=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_fuzz_mul_div_rounding_up(x: int, y: int, d: int):
    hypothesis.assume(d != 0)
    hypothesis.assume(y != 0)
    hypothesis.assume(0 <= x <= MAX_UINT256 // y)

    numerator: int = x * y
    result: int = muldiv_rounding_up(x, y, d)
    if mulmod(x, y, d) > 0:
        assert result == numerator // d + 1
    else:
        assert result == numerator // d


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    d=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_invariant_mul_div_rounding(x: int, y: int, d: int):
    hypothesis.assume(d > 0)
    hypothesis.assume(not result_overflows(x, y, d))

    ceiled = muldiv_rounding_up(x, y, d)
    floored = muldiv(x, y, d)
    if mulmod(x, y, d) > 0:
        assert ceiled - floored == 1
    else:
        assert ceiled == floored


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    d=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_invariant_mul_div(x: int, y: int, d: int):
    hypothesis.assume(d > 0)
    hypothesis.assume(not result_overflows(x, y, d))

    z = muldiv(x, y, d)
    if x == 0 or y == 0:
        assert z == 0

        return

    # recompute x and y via mulDiv of the result of floor(x*y/d), should always be less than
    # original inputs by < d
    x2 = muldiv(z, d, y)
    y2 = muldiv(z, d, x)
    assert x2 <= x
    assert y2 <= y
    assert x - x2 < d
    assert y - y2 < d


@hypothesis.given(
    x=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    y=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
    d=hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    ),
)
def test_invariant_mul_div_rounding_up(x: int, y: int, d: int):
    hypothesis.assume(d > 0)
    hypothesis.assume(not result_overflows(x, y, d))

    z = muldiv_rounding_up(x, y, d)
    if x == 0 or y == 0:
        assert z == 0
        return

    hypothesis.assume(not result_overflows(z, d, y))
    hypothesis.assume(not result_overflows(z, d, x))

    # recompute x and y via mulDiv of the result of ceil(x*y/d),
    # should always be greater than original inputs by < d
    x2 = muldiv(z, d, y)
    y2 = muldiv(z, d, x)
    assert x2 >= x
    assert y2 >= y
    assert x2 - x < d
    assert y2 - y < d


def test_result_overflows_helper():
    assert not result_overflows(0, 0, 1)
    assert not result_overflows(1, 0, 1)
    assert not result_overflows(0, 1, 1)
    assert not result_overflows(1, 1, 1)
    assert not result_overflows(10000000, 10000000, 1)
    assert not result_overflows(Q128, 50 * Q128 // 100, 150 * Q128 // 100)
    assert not result_overflows(Q128, 35 * Q128, 8 * Q128)
    assert result_overflows(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)
    assert result_overflows(Q128, MAX_UINT256, 1)


def result_overflows(x: int, y: int, d: int) -> bool:
    assert d > 0

    # If x or y is zero, the result will be zero, and there's no overflow
    if x == 0 or y == 0:
        return False

    # If intermediate multiplication doesn't overflow, there's no overflow
    if x * y <= MAX_UINT256:
        return False

    try:
        muldiv(x, y, d)
    except ValidationError:
        return True

    try:
        muldiv_rounding_up(x, y, d)
    except ValidationError:
        return True

    return False
