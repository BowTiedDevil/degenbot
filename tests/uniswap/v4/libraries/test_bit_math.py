import hypothesis
import hypothesis.strategies
import pytest

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.uniswap.v4_libraries import bit_math

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/BitMath.t.sol


def _most_significant_bit_reference(x: int):
    i = 0
    while (x := x >> 1) > 0:
        i += 1
    return i


def _least_significant_bit_reference(x: int):
    assert x > 0, "BitMath: zero has no least significant bit"

    i = 0
    while (x >> i) & 1 == 0:
        i += 1
    return i


def test_most_significant_bit_reverts_when_zero():
    with pytest.raises(ValueError, match="Number must be >0"):
        bit_math.most_significant_bit(0)


def test_most_significant_bit_one():
    assert bit_math.most_significant_bit(1) == 0


def test_most_significant_bit_two():
    assert bit_math.most_significant_bit(2) == 1


def test_most_significant_bit_powers_of_two():
    for i in range(255):
        assert bit_math.most_significant_bit(1 << i) == i


def test_most_significant_bit_max_uint256():
    assert bit_math.most_significant_bit(MAX_UINT256) == 255


@hypothesis.given(
    hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    )
)
def test_invariant_most_significant_bit(x: int):
    hypothesis.assume(x != 0)
    msb = bit_math.most_significant_bit(x)
    assert x >= (2**msb)
    assert msb == 255 or x < (2 ** (msb + 1))


@hypothesis.given(
    hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    )
)
def test_fuzz_most_significant_bit(x: int):
    hypothesis.assume(x != 0)
    assert bit_math.most_significant_bit(x) == _most_significant_bit_reference(x)


def test_least_significant_bit_reverts_when_zero():
    with pytest.raises(ValueError, match="Number must be >0"):
        bit_math.least_significant_bit(0)


def test_least_significant_bit_one():
    assert bit_math.least_significant_bit(1) == 0


def test_least_significant_bit_two():
    assert bit_math.least_significant_bit(2) == 1


def test_least_significant_bit_powers_of_two():
    for i in range(255):
        x = 1 << i
        assert bit_math.least_significant_bit(x) == i


def test_least_significant_bit_max_uint256():
    assert bit_math.least_significant_bit(MAX_UINT256) == 0


@hypothesis.given(
    hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    )
)
def test_fuzz_least_significant_bit(x: int):
    hypothesis.assume(x != 0)
    assert bit_math.least_significant_bit(x) == _least_significant_bit_reference(x)


@hypothesis.given(
    hypothesis.strategies.integers(
        min_value=MIN_UINT256,
        max_value=MAX_UINT256,
    )
)
def test_invariant_least_significant_bit(x: int):
    hypothesis.assume(x != 0)
    lsb = bit_math.least_significant_bit(x)
    assert x & (2**lsb) != 0
    assert x & (2**lsb - 1) == 0


@hypothesis.given(
    number=hypothesis.strategies.integers(min_value=1),
)
def test_least_significant_bit_equivalence(number):
    assert bit_math.least_significant_bit_legacy(number) == bit_math.least_significant_bit(number)


@hypothesis.given(
    number=hypothesis.strategies.integers(min_value=1),
)
def test_most_significant_bit_equivalence(number):
    assert bit_math.most_significant_bit_legacy(number) == bit_math.most_significant_bit(number)
