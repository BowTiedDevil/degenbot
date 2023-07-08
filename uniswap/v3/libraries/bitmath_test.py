import pytest

from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import BitMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/BitMath.spec.ts


def test_mostSignificantBit():
    with pytest.raises(EVMRevertError):
        # this test should fail
        BitMath.mostSignificantBit(0)

    assert BitMath.mostSignificantBit(1) == 0

    assert BitMath.mostSignificantBit(2) == 1

    for i in range(256):
        # test all powers of 2
        assert BitMath.mostSignificantBit(2**i) == i
    assert BitMath.mostSignificantBit(2**256 - 1) == 255


def test_leastSignificantBit():
    with pytest.raises(EVMRevertError):
        # this test should fail
        BitMath.leastSignificantBit(0)

    assert BitMath.leastSignificantBit(1) == 0

    assert BitMath.leastSignificantBit(2) == 1

    for i in range(256):
        # test all powers of 2
        assert BitMath.leastSignificantBit(2**i) == i

    assert BitMath.leastSignificantBit(2**256 - 1) == 0
