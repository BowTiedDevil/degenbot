import pytest

from degenbot.constants import MAX_INT128, MAX_UINT128, MIN_INT128, MIN_UINT128
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries import LiquidityMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/LiquidityMath.spec.ts


def test_addDelta():
    ### ----------------------------------------------------
    ### LiquidityMath tests
    ### ----------------------------------------------------

    assert LiquidityMath.addDelta(1, 0) == 1
    assert LiquidityMath.addDelta(1, -1) == 0
    assert LiquidityMath.addDelta(1, 1) == 2

    with pytest.raises(EVMRevertError):
        LiquidityMath.addDelta(MIN_UINT128 - 1, 0)

    with pytest.raises(EVMRevertError):
        LiquidityMath.addDelta(MAX_UINT128 + 1, 0)

    with pytest.raises(EVMRevertError):
        LiquidityMath.addDelta(0, MIN_INT128 - 1)

    with pytest.raises(EVMRevertError):
        LiquidityMath.addDelta(0, MAX_INT128 + 1)

    with pytest.raises(EVMRevertError, match="LA"):
        # 2**128-15 + 15 overflows
        LiquidityMath.addDelta(2**128 - 15, 15)

    with pytest.raises(EVMRevertError, match="LS"):
        # 0 + -1 underflows
        LiquidityMath.addDelta(0, -1)

    with pytest.raises(EVMRevertError, match="LS"):
        # 3 + -4 underflows
        LiquidityMath.addDelta(3, -4)
