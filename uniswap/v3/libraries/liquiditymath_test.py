import pytest

from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import LiquidityMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/LiquidityMath.spec.ts


def test_addDelta():
    ### ----------------------------------------------------
    ### LiquidityMath tests
    ### ----------------------------------------------------

    assert LiquidityMath.addDelta(1, 0) == 1
    assert LiquidityMath.addDelta(1, -1) == 0
    assert LiquidityMath.addDelta(1, 1) == 2

    with pytest.raises(EVMRevertError, match="LA"):
        # 2**128-15 + 15 overflows
        LiquidityMath.addDelta(2**128 - 15, 15)

    with pytest.raises(EVMRevertError, match="LS"):
        # 0 + -1 underflows
        LiquidityMath.addDelta(0, -1)

    with pytest.raises(EVMRevertError, match="LS"):
        # 3 + -4 underflows
        LiquidityMath.addDelta(3, -4)
