import hypothesis
import hypothesis.strategies

from degenbot.constants import MAX_INT128, MAX_UINT128, MAX_UINT256, MIN_UINT256
from degenbot.uniswap.v4_libraries.unsafe_math import div_rounding_up, simple_mul_div

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/UnsafeMath.t.sol


Q128 = 2**128


@hypothesis.given(
    x=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_div_rounding_up_zero_does_not_revert(x: int):
    div_rounding_up(x, 0)


def test_div_rounding_up_max_input():
    assert div_rounding_up(MAX_UINT256, MAX_UINT256) == 1


def test_div_rounding_up_rounds_up():
    assert div_rounding_up(Q128, 3) == Q128 // 3 + 1


@hypothesis.given(
    x=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    y=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_fuzz_div_rounding_up(x: int, y: int):
    hypothesis.assume(y != 0)

    result = div_rounding_up(x, y)
    assert result == x // y or result == x // y + 1


@hypothesis.given(
    x=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    y=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_invariant_div_rounding_up(x: int, y: int):
    hypothesis.assume(y != 0)

    z = div_rounding_up(x, y)
    diff = z - (x // y)
    if x % y == 0:
        assert diff == 0
    else:
        assert diff == 1


@hypothesis.given(
    a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    b=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_simple_mul_div_zero_does_not_revert(a: int, b: int):
    simple_mul_div(a, b, 0)


def test_simple_mul_div_succeeds():
    assert simple_mul_div(Q128, 3, 2) == Q128 * 3 // 2


def test_simple_mul_div_no_overflow():
    # NOTE: test does not actually call simple_mul_div
    # TODO: file issue
    assert MAX_INT128 * Q128 <= MAX_UINT256


@hypothesis.given(
    a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    b=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    denominator=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_fuzz_simple_mul_div_succeeds(a: int, b: int, denominator: int):
    hypothesis.assume(denominator != 0)
    hypothesis.assume(a * b <= MAX_UINT256)

    assert simple_mul_div(a, b, denominator) == (a * b) // denominator


@hypothesis.given(
    a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    denominator=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_fuzz_simple_mul_div_bounded(a: int, denominator: int):
    hypothesis.assume(0 <= a <= MAX_INT128)
    hypothesis.assume(1 <= denominator <= MAX_UINT128)

    assert simple_mul_div(a, Q128, denominator) == a * Q128 // denominator
