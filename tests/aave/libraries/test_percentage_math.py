import pytest
from hypothesis import given, strategies

from degenbot.aave.libraries.percentage_math import (
    HALF_PERCENTAGE_FACTOR,
    PERCENTAGE_FACTOR,
    percent_div,
    percent_div_ceil,
    percent_mul,
    percent_mul_ceil,
    percent_mul_floor,
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


def test_percent_mul_ceil_exact() -> None:
    assert percent_mul_ceil(100 * 10**18, PERCENTAGE_FACTOR) == 100 * 10**18


def test_percent_mul_ceil_with_rounding_up() -> None:
    assert percent_mul_ceil(1, 1) == 1


def test_percent_mul_ceil_zero_value_or_percent() -> None:
    assert percent_mul_ceil(0, 100) == 0
    assert percent_mul_ceil(100, 0) == 0


def test_percent_mul_ceil_revert_on_overflow() -> None:
    with pytest.raises(EVMRevertError):
        percent_mul_ceil(MAX_UINT256, 2)


def test_percent_mul_floor_exact() -> None:
    assert percent_mul_floor(100 * 10**18, PERCENTAGE_FACTOR) == 100 * 10**18


def test_percent_mul_floor_with_truncation() -> None:
    assert percent_mul_floor(1, 1) == 0


def test_percent_mul_floor_zero_inputs() -> None:
    assert percent_mul_floor(0, 1234) == 0
    assert percent_mul_floor(1234, 0) == 0


def test_percent_mul_floor_revert_on_overflow() -> None:
    with pytest.raises(EVMRevertError):
        percent_mul_floor(MAX_UINT256, 2)


def test_percent_div_ceil_exact() -> None:
    assert percent_div_ceil(100 * 10**18, PERCENTAGE_FACTOR) == 100 * 10**18


def test_percent_div_ceil_with_ceil_needed() -> None:
    assert percent_div_ceil(5, 3) == 16667


def test_percent_div_ceil_revert_on_div_by_zero() -> None:
    with pytest.raises(EVMRevertError):
        percent_div_ceil(1234, 0)


def test_percent_div_ceil_revert_on_overflow() -> None:
    with pytest.raises(EVMRevertError):
        percent_div_ceil(MAX_UINT256, 1)


def test_percent_mul_floor() -> None:
    assert percent_mul_floor(1 * 10**18, 5000) == 5 * 10**17
    assert percent_mul_floor(142515 * 10**14, 7442) == 10605966300000000000
    assert percent_mul_floor(9087312 * 10**27, 1333) == 1211338689600000000000000000000000


def test_percent_mul_ceil() -> None:
    assert percent_mul_ceil(1 * 10**18, 5000) == 5 * 10**17
    assert percent_mul_ceil(142515 * 10**14, 7442) == 10605966300000000000
    assert percent_mul_ceil(9087312 * 10**27, 1333) == 1211338689600000000000000000000000


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


@given(
    value=strategies.integers(min_value=0, max_value=MAX_UINT256),
    percentage=strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_percent_mul_floor_fuzz(value: int, percentage: int) -> None:
    if percentage != 0 and value > MAX_UINT256 // percentage:
        with pytest.raises(EVMRevertError):
            percent_mul_floor(value, percentage)
    else:
        assert percent_mul_floor(value, percentage) == (value * percentage) // PERCENTAGE_FACTOR


@given(
    value=strategies.integers(min_value=0, max_value=MAX_UINT256),
    percentage=strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_percent_mul_ceil_fuzz(value: int, percentage: int) -> None:
    if percentage != 0 and value > MAX_UINT256 // percentage:
        with pytest.raises(EVMRevertError):
            percent_mul_ceil(value, percentage)
    else:
        product = value * percentage
        expected = (product // PERCENTAGE_FACTOR) + (1 if product % PERCENTAGE_FACTOR != 0 else 0)
        assert percent_mul_ceil(value, percentage) == expected


@given(
    value=strategies.integers(min_value=0, max_value=MAX_UINT256),
    percentage=strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_percent_div_ceil_fuzz(value: int, percentage: int) -> None:
    if percentage == 0 or value > MAX_UINT256 // PERCENTAGE_FACTOR:
        with pytest.raises(EVMRevertError):
            percent_div_ceil(value, percentage)
    else:
        val = value * PERCENTAGE_FACTOR
        expected = (val // percentage) + (1 if val % percentage != 0 else 0)
        assert percent_div_ceil(value, percentage) == expected
