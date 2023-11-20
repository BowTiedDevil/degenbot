import random

import pytest
from degenbot.constants import MAX_UINT256
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries import FullMath
from degenbot.uniswap.v3_libraries.constants import Q128

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/FullMath.spec.ts


def test_mulDiv():
    ### ----------------------------------------------------
    ### FullMath tests
    ### ----------------------------------------------------

    # mulDiv tests
    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(Q128, 5, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(Q128, Q128, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(Q128, Q128, 1)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)

    assert FullMath.mulDiv(MAX_UINT256, MAX_UINT256, MAX_UINT256) == MAX_UINT256

    assert (
        FullMath.mulDiv(
            Q128,
            50 * Q128 // 100,  # 0.5x
            150 * Q128 // 100,  # 1.5x
        )
        == Q128 // 3
    )

    assert FullMath.mulDiv(Q128, 35 * Q128, 8 * Q128) == 4375 * Q128 // 1000

    assert (
        FullMath.mulDiv(
            Q128,
            1000 * Q128,
            3000 * Q128,
        )
        == Q128 // 3
    )

    with pytest.raises(EVMRevertError):
        FullMath.mulDiv(-1, Q128, Q128)

    with pytest.raises(EVMRevertError):
        FullMath.mulDiv(Q128, -1, Q128)


def test_mulDivRoundingUp():
    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(Q128, 5, 0)

    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(Q128, Q128, 0)

    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(Q128, Q128, 1)

    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)

    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(
            535006138814359,
            432862656469423142931042426214547535783388063929571229938474969,
            2,
        )

    with pytest.raises(EVMRevertError):
        FullMath.mulDivRoundingUp(
            115792089237316195423570985008687907853269984659341747863450311749907997002549,
            115792089237316195423570985008687907853269984659341747863450311749907997002550,
            115792089237316195423570985008687907853269984653042931687443039491902864365164,
        )

    # all max inputs
    assert FullMath.mulDivRoundingUp(MAX_UINT256, MAX_UINT256, MAX_UINT256) == MAX_UINT256

    # accurate without phantom overflow
    assert (
        FullMath.mulDivRoundingUp(
            Q128,
            50 * Q128 // 100,
            150 * Q128 // 100,
        )
        == Q128 // 3 + 1
    )

    # accurate with phantom overflow
    assert FullMath.mulDivRoundingUp(Q128, 35 * Q128, 8 * Q128) == 4375 * Q128 // 1000

    # accurate with phantom overflow and repeating decimal
    assert (
        FullMath.mulDivRoundingUp(
            Q128,
            1000 * Q128,
            3000 * Q128,
        )
        == Q128 // 3 + 1
    )

    def pseudoRandomBigNumber():
        return int(MAX_UINT256 * random.random())

    def floored(x, y, d):
        return FullMath.mulDiv(x, y, d)

    def ceiled(x, y, d):
        return FullMath.mulDivRoundingUp(x, y, d)

    for i in range(1000):
        # override x, y for first two runs to cover the x == 0 and y == 0 cases
        x = pseudoRandomBigNumber() if i != 0 else 0
        y = pseudoRandomBigNumber() if i != 1 else 0
        d = pseudoRandomBigNumber()

        if x == 0 or y == 0:
            assert floored(x, y, d) == 0
            assert ceiled(x, y, d) == 0
        elif x * y // d > MAX_UINT256:
            with pytest.raises(EVMRevertError):
                # this test should fail
                floored(x, y, d)
            with pytest.raises(EVMRevertError):
                # this test should fail
                ceiled(x, y, d)
        else:
            assert floored(x, y, d) == x * y // d
            assert ceiled(x, y, d) == x * y // d + (1 if (x * y % d > 0) else 0)
