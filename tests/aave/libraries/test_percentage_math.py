import pytest
from hypothesis import given, strategies

from degenbot.aave.libraries.percentage_math import (
    HALF_PERCENTAGE_FACTOR,
    PERCENTAGE_FACTOR,
    percent_div,
    percent_mul,
)
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError


def test_percent_mul() -> None:
    assert percent_mul(1 * 10**18, 5000) == 5 * 10**17
    assert percent_mul(142515 * 10**14, 7442) == 10605966300000000000
    assert percent_mul(9087312 * 10**27, 1333) == 1211338689600000000000000000000000


def test_percent_div() -> None:
    assert percent_div(1 * 10**18, 5000) == 2 * 10**18
    assert percent_div(142515 * 10**14, 7442) == 19150094060736361193
    assert percent_div(9087312 * 10**27, 1333) == 68171882970742685671417854463615904


@given(
    value=strategies.integers(min_value=0, max_value=MAX_UINT256),
    percentage=strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_percent_mul_fuzz(value: int, percentage: int) -> None:
    if not (percentage == 0 or not (value > (MAX_UINT256 - HALF_PERCENTAGE_FACTOR) // percentage)):
        with pytest.raises(EVMRevertError):
            percent_mul(value, percentage)
    else:
        assert (
            percent_mul(value, percentage)
            == ((value * percentage) + HALF_PERCENTAGE_FACTOR) // PERCENTAGE_FACTOR
        )


@given(
    value=strategies.integers(min_value=0, max_value=MAX_UINT256),
    percentage=strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_percent_div_fuzz(value: int, percentage: int) -> None:
    if percentage == 0 or (value > (MAX_UINT256 - (percentage // 2)) // PERCENTAGE_FACTOR):
        with pytest.raises(EVMRevertError):
            percent_div(value, percentage)
    else:
        assert (
            percent_div(value, percentage)
            == ((value * PERCENTAGE_FACTOR) + (percentage // 2)) // percentage
        )
