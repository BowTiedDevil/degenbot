import hypothesis
import hypothesis.strategies
import pytest

from degenbot.aave.libraries.v3_5.wad_ray_math import (
    HALF_RAY,
    HALF_WAD,
    RAY,
    WAD,
    WAD_RAY_RATIO,
    Rounding,
    ray_div,
    ray_div_ceil,
    ray_div_floor,
    ray_mul,
    ray_mul_ceil,
    ray_mul_floor,
    ray_to_wad,
    wad_div,
    wad_mul,
    wad_to_ray,
)
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError


def test_wad_mul_basic() -> None:
    assert wad_mul(WAD, WAD) == WAD
    assert wad_mul(2 * WAD, 3 * WAD) == 6 * WAD


def test_wad_mul_zero() -> None:
    assert wad_mul(0, WAD) == 0
    assert wad_mul(WAD, 0) == 0
    assert wad_mul(0, 0) == 0


def test_wad_mul_half_up_rounding() -> None:
    assert wad_mul(WAD + HALF_WAD, WAD) == WAD + WAD // 2
    assert wad_mul(WAD + HALF_WAD - 1, WAD) == WAD + WAD // 2 - 1
    assert wad_mul(WAD, WAD + HALF_WAD) == WAD + WAD // 2


def test_wad_mul_overflow() -> None:
    large_value = (MAX_UINT256 - HALF_WAD) // WAD + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        wad_mul(large_value, WAD)


def test_wad_div_basic() -> None:
    assert wad_div(WAD, WAD) == WAD
    assert wad_div(6 * WAD, 3 * WAD) == 2 * WAD
    assert wad_div(WAD, 2 * WAD) == WAD // 2


def test_wad_div_zero() -> None:
    assert wad_div(0, WAD) == 0
    with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
        wad_div(WAD, 0)


def test_wad_div_half_up_rounding() -> None:
    assert wad_div(WAD + WAD // 2, WAD) == WAD + WAD // 2
    assert wad_div(WAD + WAD // 2 - 1, WAD) == WAD + WAD // 2 - 1


def test_wad_div_overflow() -> None:
    large_value = (MAX_UINT256 - (WAD // 2)) // WAD + 1
    with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
        wad_div(large_value, WAD)


def test_ray_mul_basic() -> None:
    assert ray_mul(RAY, RAY) == RAY
    assert ray_mul(2 * RAY, 3 * RAY) == 6 * RAY


def test_ray_mul_zero() -> None:
    assert ray_mul(0, RAY) == 0
    assert ray_mul(RAY, 0) == 0
    assert ray_mul(0, 0) == 0


def test_ray_mul_half_up_rounding() -> None:
    assert ray_mul(RAY + HALF_RAY, RAY) == RAY + RAY // 2
    assert ray_mul(RAY + HALF_RAY - 1, RAY) == RAY + RAY // 2 - 1


def test_ray_mul_overflow() -> None:
    large_value = (MAX_UINT256 - HALF_RAY) // RAY + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        ray_mul(large_value, RAY)


def test_ray_mul_floor_basic() -> None:
    assert ray_mul_floor(RAY, RAY) == RAY
    assert ray_mul_floor(2 * RAY, 3 * RAY) == 6 * RAY


def test_ray_mul_floor_rounding() -> None:
    assert ray_mul_floor(RAY + HALF_RAY, RAY) == RAY + HALF_RAY
    assert ray_mul_floor(RAY + HALF_RAY - 1, RAY) == RAY + HALF_RAY - 1


def test_ray_mul_floor_overflow() -> None:
    large_value = (MAX_UINT256 // RAY) + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        ray_mul_floor(large_value, RAY)


def test_ray_mul_ceil_basic() -> None:
    assert ray_mul_ceil(RAY, RAY) == RAY
    assert ray_mul_ceil(2 * RAY, 3 * RAY) == 6 * RAY


def test_ray_mul_ceil_rounding() -> None:
    assert ray_mul_ceil(RAY + HALF_RAY, RAY) == RAY + HALF_RAY
    assert ray_mul_ceil(RAY + HALF_RAY - 1, RAY) == RAY + HALF_RAY - 1 - 1 + 1


def test_ray_mul_ceil_exact_value() -> None:
    assert ray_mul_ceil(RAY, RAY) == RAY
    assert ray_mul_ceil(2 * RAY, RAY) == 2 * RAY


def test_ray_mul_ceil_overflow() -> None:
    large_value = (MAX_UINT256 // RAY) + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        ray_mul_ceil(large_value, RAY)


def test_ray_mul_floor_vs_ceil() -> None:
    assert ray_mul_floor(RAY, RAY) == RAY
    assert ray_mul_ceil(RAY, RAY) == RAY
    assert ray_mul_floor(2 * RAY, RAY) == 2 * RAY
    assert ray_mul_ceil(2 * RAY, RAY) == 2 * RAY


def test_ray_mul_with_rounding_enum() -> None:
    value = RAY + (RAY // 2)
    assert ray_mul(value, 1, Rounding.FLOOR) == ray_mul_floor(value, 1)
    assert ray_mul(value, 1, Rounding.CEIL) == ray_mul_ceil(value, 1)


def test_ray_div_basic() -> None:
    assert ray_div(RAY, RAY) == RAY
    assert ray_div(6 * RAY, 3 * RAY) == 2 * RAY
    assert ray_div(RAY, 2 * RAY) == RAY // 2


def test_ray_div_zero() -> None:
    assert ray_div(0, RAY) == 0
    with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
        ray_div(RAY, 0)


def test_ray_div_half_up_rounding() -> None:
    assert ray_div(RAY + RAY // 2, RAY) == RAY + RAY // 2
    assert ray_div(RAY + RAY // 2 - 1, RAY) == RAY + RAY // 2 - 1


def test_ray_div_overflow() -> None:
    large_value = (MAX_UINT256 - (RAY // 2)) // RAY + 1
    with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
        ray_div(large_value, RAY)


def test_ray_div_floor_basic() -> None:
    assert ray_div_floor(RAY, RAY) == RAY
    assert ray_div_floor(6 * RAY, 3 * RAY) == 2 * RAY


def test_ray_div_floor_rounding() -> None:
    assert ray_div_floor(3 * RAY // 2, RAY) == RAY + HALF_RAY
    assert ray_div_floor(3 * RAY // 2 - 1, RAY) == 3 * RAY // 2 - 1


def test_ray_div_floor_zero_division() -> None:
    with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
        ray_div_floor(RAY, 0)


def test_ray_div_floor_overflow() -> None:
    large_value = (MAX_UINT256 // RAY) + 1
    with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
        ray_div_floor(large_value, RAY)


def test_ray_div_ceil_basic() -> None:
    assert ray_div_ceil(RAY, RAY) == RAY
    assert ray_div_ceil(6 * RAY, 3 * RAY) == 2 * RAY


def test_ray_div_ceil_rounding() -> None:
    assert ray_div_ceil(3 * RAY // 2, RAY) == RAY + HALF_RAY
    assert ray_div_ceil(3 * RAY // 2 - 1, RAY) == 3 * RAY // 2 - 1


def test_ray_div_ceil_exact_value() -> None:
    assert ray_div_ceil(2 * RAY, RAY) == 2 * RAY


def test_ray_div_ceil_zero_division() -> None:
    with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
        ray_div_ceil(RAY, 0)


def test_ray_div_ceil_overflow() -> None:
    large_value = (MAX_UINT256 // RAY) + 1
    with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
        ray_div_ceil(large_value, RAY)


def test_ray_div_floor_vs_ceil() -> None:
    assert ray_div_floor(RAY, RAY) == RAY
    assert ray_div_ceil(RAY, RAY) == RAY
    assert ray_div_floor(2 * RAY, RAY) == 2 * RAY
    assert ray_div_ceil(2 * RAY, RAY) == 2 * RAY


def test_ray_div_with_rounding_enum() -> None:
    assert ray_div(3 * RAY // 2, RAY, Rounding.FLOOR) == ray_div_floor(3 * RAY // 2, RAY)
    assert ray_div(3 * RAY // 2, RAY, Rounding.CEIL) == ray_div_ceil(3 * RAY // 2, RAY)


def test_ray_to_wad_basic() -> None:
    assert ray_to_wad(RAY) == WAD
    assert ray_to_wad(2 * RAY) == 2 * WAD
    assert ray_to_wad(0) == 0


def test_ray_to_wad_half_up_rounding() -> None:
    half_ratio = WAD_RAY_RATIO // 2
    assert ray_to_wad(RAY + half_ratio) == WAD + 1
    assert ray_to_wad(RAY + half_ratio - 1) == WAD


def test_ray_to_wad_large_values() -> None:
    large_ray = 1000 * RAY
    assert ray_to_wad(large_ray) == 1000 * WAD


def test_wad_to_ray_basic() -> None:
    assert wad_to_ray(WAD) == RAY
    assert wad_to_ray(2 * WAD) == 2 * RAY
    assert wad_to_ray(0) == 0


def test_wad_to_ray_overflow() -> None:
    large_value = (MAX_UINT256 // WAD_RAY_RATIO) + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        wad_to_ray(large_value)


def test_conversion_round_trip() -> None:
    original = 12345 * RAY
    assert wad_to_ray(ray_to_wad(original)) == original


def test_exact_values_floor_ceil_same() -> None:
    value = 2 * RAY
    assert ray_mul_floor(value, RAY) == value
    assert ray_mul_ceil(value, RAY) == value
    assert ray_mul(value, RAY, Rounding.FLOOR) == value
    assert ray_mul(value, RAY, Rounding.CEIL) == value


def test_exact_values_div_floor_ceil_same() -> None:
    value = 2 * RAY
    assert ray_div_floor(value, RAY) == value
    assert ray_div_ceil(value, RAY) == value
    assert ray_div(value, RAY, Rounding.FLOOR) == value
    assert ray_div(value, RAY, Rounding.CEIL) == value


@hypothesis.given(
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
)
@hypothesis.example(0, 0)
@hypothesis.example(WAD, 0)
@hypothesis.example(0, WAD)
def test_wad_mul_fuzzing(a: int, b: int) -> None:
    if b != 0 and a > (MAX_UINT256 - HALF_WAD) // b:
        with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
            wad_mul(a, b)
    else:
        expected = (a * b + HALF_WAD) // WAD
        assert wad_mul(a, b) == expected


@hypothesis.given(
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_wad_div_fuzzing(a: int, b: int) -> None:
    if b == 0:
        with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
            wad_div(a, b)
    elif a > (MAX_UINT256 - (b // 2)) // WAD:
        with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
            wad_div(a, b)
    else:
        expected = (a * WAD + (b // 2)) // b
        assert wad_div(a, b) == expected


@hypothesis.given(hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256))
@hypothesis.example(MAX_UINT256 // WAD_RAY_RATIO)
def test_wad_to_ray_fuzzing(a: int) -> None:
    if a > MAX_UINT256 // WAD_RAY_RATIO:
        with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
            wad_to_ray(a)
    else:
        expected = a * WAD_RAY_RATIO
        assert wad_to_ray(a) == expected


@hypothesis.given(hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256))
@hypothesis.example(RAY)
@hypothesis.example(2 * RAY)
@hypothesis.example(RAY + WAD_RAY_RATIO // 2)
def test_ray_to_wad_fuzzing(a: int) -> None:
    result = a // WAD_RAY_RATIO
    remainder = a % WAD_RAY_RATIO
    expected = result + 1 if remainder >= WAD_RAY_RATIO // 2 else result
    assert ray_to_wad(a) == expected


@hypothesis.given(
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
)
@hypothesis.example(0, 0)
@hypothesis.example(RAY, 0)
@hypothesis.example(0, RAY)
def test_ray_mul_fuzzing(a: int, b: int) -> None:
    if b != 0 and a > (MAX_UINT256 - HALF_RAY) // b:
        with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
            ray_mul(a, b)
    else:
        expected = (a * b + HALF_RAY) // RAY
        assert ray_mul(a, b) == expected


@hypothesis.given(
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
    hypothesis.strategies.integers(min_value=0, max_value=MAX_UINT256),
)
def test_ray_div_fuzzing(a: int, b: int) -> None:
    if b == 0:
        with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
            ray_div(a, b)
    elif a > (MAX_UINT256 - (b // 2)) // RAY:
        with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
            ray_div(a, b)
    else:
        expected = (a * RAY + (b // 2)) // b
        assert ray_div(a, b) == expected


def test_wad_mul_foundry_cases() -> None:
    assert wad_mul(2500000000000000000000, 500000000000000000000) == 1250000000000000000000000
    assert wad_mul(412200000000000000000000, WAD) == 412200000000000000000000
    assert wad_mul(6 * WAD, 2 * WAD) == 12 * WAD


def test_wad_div_foundry_cases() -> None:
    assert wad_div(2500000000000000000, 500000000000000000) == 5000000000000000000
    assert wad_div(412200000000000000000, WAD) == 412200000000000000000
    assert wad_div(8745000000000000000, 670000000000000000) == 13052238805970149254


def test_ray_mul_foundry_cases() -> None:
    assert (
        ray_mul(2500000000000000000000000000, 500000000000000000000000000)
        == 1250000000000000000000000000
    )
    assert ray_mul(412200000000000000000000000000, RAY) == 412200000000000000000000000000
    assert ray_mul(6 * RAY, 2 * RAY) == 12 * RAY


def test_ray_div_foundry_cases() -> None:
    assert (
        ray_div(2500000000000000000000000000, 500000000000000000000000000)
        == 5000000000000000000000000000
    )
    assert ray_div(412200000000000000000000000000, RAY) == 412200000000000000000000000000
    assert (
        ray_div(8745000000000000000000000000, 670000000000000000000000000)
        == 13052238805970149253731343284
    )


def test_wad_to_ray_foundry_cases() -> None:
    assert wad_to_ray(WAD) == RAY
    assert wad_to_ray(412200000000000000000000) == 412200000000000000000000000000000
    assert wad_to_ray(0) == 0


def test_ray_to_wad_foundry_cases() -> None:
    assert ray_to_wad(RAY) == WAD
    assert ray_to_wad(412200000000000000000000000000000) == 412200000000000000000000
    assert ray_to_wad(0) == 0
