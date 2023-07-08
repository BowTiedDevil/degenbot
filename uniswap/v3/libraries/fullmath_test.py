import random

import pytest

from degenbot.constants import MAX_UINT256
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import FixedPoint128, FullMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/FullMath.spec.ts


def test_mulDiv():
    ### ----------------------------------------------------
    ### FullMath tests
    ### ----------------------------------------------------

    # mulDiv tests
    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(FixedPoint128.Q128, 5, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(FixedPoint128.Q128, FixedPoint128.Q128, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(FixedPoint128.Q128, FixedPoint128.Q128, 1)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDiv(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)

    assert (
        FullMath.mulDiv(MAX_UINT256, MAX_UINT256, MAX_UINT256) == MAX_UINT256
    )

    assert (
        FullMath.mulDiv(
            FixedPoint128.Q128,
            50 * FixedPoint128.Q128 // 100,  # 0.5x
            150 * FixedPoint128.Q128 // 100,  # 1.5x
        )
        == FixedPoint128.Q128 // 3
    )

    assert (
        FullMath.mulDiv(
            FixedPoint128.Q128, 35 * FixedPoint128.Q128, 8 * FixedPoint128.Q128
        )
        == 4375 * FixedPoint128.Q128 // 1000
    )

    assert (
        FullMath.mulDiv(
            FixedPoint128.Q128,
            1000 * FixedPoint128.Q128,
            3000 * FixedPoint128.Q128,
        )
        == FixedPoint128.Q128 // 3
    )


def test_mulDivRoundingUp():
    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(FixedPoint128.Q128, 5, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(FixedPoint128.Q128, FixedPoint128.Q128, 0)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(FixedPoint128.Q128, FixedPoint128.Q128, 1)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(MAX_UINT256, MAX_UINT256, MAX_UINT256 - 1)

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(
            535006138814359,
            432862656469423142931042426214547535783388063929571229938474969,
            2,
        )

    with pytest.raises(EVMRevertError):
        # this test should fail
        FullMath.mulDivRoundingUp(
            115792089237316195423570985008687907853269984659341747863450311749907997002549,
            115792089237316195423570985008687907853269984659341747863450311749907997002550,
            115792089237316195423570985008687907853269984653042931687443039491902864365164,
        )

    # all max inputs
    assert (
        FullMath.mulDivRoundingUp(MAX_UINT256, MAX_UINT256, MAX_UINT256)
        == MAX_UINT256
    )

    # accurate without phantom overflow
    assert (
        FullMath.mulDivRoundingUp(
            FixedPoint128.Q128,
            50 * FixedPoint128.Q128 // 100,
            150 * FixedPoint128.Q128 // 100,
        )
        == FixedPoint128.Q128 // 3 + 1
    )

    # accurate with phantom overflow
    assert (
        FullMath.mulDivRoundingUp(
            FixedPoint128.Q128, 35 * FixedPoint128.Q128, 8 * FixedPoint128.Q128
        )
        == 4375 * FixedPoint128.Q128 // 1000
    )

    # accurate with phantom overflow and repeating decimal
    assert (
        FullMath.mulDivRoundingUp(
            FixedPoint128.Q128,
            1000 * FixedPoint128.Q128,
            3000 * FixedPoint128.Q128,
        )
        == FixedPoint128.Q128 // 3 + 1
    )

    def pseudoRandomBigNumber():
        return int(MAX_UINT256 * random.random())

    def floored(x, y, d):
        return FullMath.mulDiv(x, y, d)

    def ceiled(x, y, d):
        return FullMath.mulDivRoundingUp(x, y, d)

    for _ in range(1000):
        x = pseudoRandomBigNumber()
        y = pseudoRandomBigNumber()
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
            assert ceiled(x, y, d) == x * y // d + (
                1 if (x * y % d > 0) else 0
            )
