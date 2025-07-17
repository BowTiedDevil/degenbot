import pytest

from degenbot.constants import MAX_INT128, MAX_UINT128, MIN_INT128, MIN_UINT128
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.v3_libraries.liquidity_math import add_delta

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/LiquidityMath.spec.ts


def test_add_delta():
    assert add_delta(1, 0) == 1
    assert add_delta(1, -1) == 0
    assert add_delta(1, 1) == 2

    with pytest.raises(EVMRevertError):
        add_delta(MIN_UINT128 - 1, 0)

    with pytest.raises(EVMRevertError):
        add_delta(MAX_UINT128 + 1, 0)

    with pytest.raises(EVMRevertError):
        add_delta(0, MIN_INT128 - 1)

    with pytest.raises(EVMRevertError):
        add_delta(0, MAX_INT128 + 1)

    with pytest.raises(EVMRevertError, match="LA"):
        # 2**128-15 + 15 overflows
        add_delta(2**128 - 15, 15)

    with pytest.raises(EVMRevertError, match="LS"):
        # 0 + -1 underflows
        add_delta(0, -1)

    with pytest.raises(EVMRevertError, match="LS"):
        # 3 + -4 underflows
        add_delta(3, -4)
