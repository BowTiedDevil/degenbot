import pytest

from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3.libraries import LiquidityMath


def test_liquiditymath():
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
        # 3 + -4 underflows
        LiquidityMath.addDelta(3, -4)
