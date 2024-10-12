import pytest

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries.bit_math import least_significant_bit, most_significant_bit

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/BitMath.spec.ts


def test_most_significant_bit():
    with pytest.raises(EVMRevertError):
        most_significant_bit(MIN_UINT256)

    with pytest.raises(EVMRevertError):
        most_significant_bit(MAX_UINT256 + 1)

    assert most_significant_bit(1) == 0
    assert most_significant_bit(2) == 1

    # Test all powers of 2
    for i in range(256):
        assert most_significant_bit(2**i) == i
    assert most_significant_bit(MAX_UINT256) == 255


def test_least_significant_bit():
    with pytest.raises(EVMRevertError):
        least_significant_bit(MIN_UINT256)

    with pytest.raises(EVMRevertError):
        least_significant_bit(MAX_UINT256 + 1)

    assert least_significant_bit(1) == 0
    assert least_significant_bit(2) == 1

    # Test all powers of 2
    for i in range(256):
        assert least_significant_bit(2**i) == i

    assert least_significant_bit(MAX_UINT256) == 0
