import pytest

from degenbot.constants import MAX_INT128, MAX_INT256, MIN_INT128, MIN_INT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.v3_libraries import functions


def test_mulmod():
    with pytest.raises(EVMRevertError):
        functions.mulmod(1, 2, 0)

    for x in range(1, 10):
        for y in range(1, 10):
            for z in range(1, 10):
                functions.mulmod(x, y, z)


def test_to_int():
    functions.to_int128(MIN_INT128)
    functions.to_int128(MAX_INT128)

    functions.to_int256(MIN_INT256)
    functions.to_int256(MAX_INT256)

    with pytest.raises(EVMRevertError):
        functions.to_int128(MIN_INT256)

    with pytest.raises(EVMRevertError):
        functions.to_int128(MAX_INT256)

    with pytest.raises(EVMRevertError):
        functions.to_int256(MIN_INT256 - 1)

    with pytest.raises(EVMRevertError):
        functions.to_int256(MAX_INT256 + 1)
