import pytest
from hypothesis import given, settings
from hypothesis.strategies import integers

from degenbot.aave.libraries.v3_4 import (
    ray_div,
    ray_mul,
    ray_to_wad,
    wad_div,
    wad_mul,
    wad_to_ray,
)
from degenbot.aave.libraries.v3_4.constants import (
    HALF_RAY,
    HALF_WAD,
    RAY,
    WAD,
    WAD_RAY_RATIO,
)
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError


def test_constants() -> None:
    assert WAD == 1 * 10**18
    assert HALF_WAD == 5 * 10**17
    assert RAY == 1 * 10**27
    assert HALF_RAY == 5 * 10**26


def test_wad_mul_edge() -> None:
    assert wad_mul(0, 1 * 10**18) == 0
    assert wad_mul(1 * 10**18, 0) == 0
    assert wad_mul(0, 0) == 0


def test_wad_mul() -> None:
    assert wad_mul(25 * 10**17, 5 * 10**17) == 125 * 10**16
    assert wad_mul(4122 * 10**17, 1 * 10**18) == 4122 * 10**17
    assert wad_mul(6 * 10**18, 2 * 10**18) == 12 * 10**18


def test_ray_mul() -> None:
    assert ray_mul(25 * 10**26, 5 * 10**26) == 125 * 10**25
    assert ray_mul(4122 * 10**26, 1 * 10**27) == 4122 * 10**26
    assert ray_mul(6 * 10**27, 2 * 10**27) == 12 * 10**27


def test_wad_div() -> None:
    assert wad_div(25 * 10**17, 5 * 10**17) == 5 * 10**18
    assert wad_div(4122 * 10**17, 1 * 10**18) == 4122 * 10**17
    assert wad_div(8745 * 10**15, 67 * 10**16) == 13052238805970149254
    assert wad_div(6 * 10**18, 2 * 10**18) == 3 * 10**18


def test_ray_div() -> None:
    assert ray_div(25 * 10**26, 5 * 10**26) == 5 * 10**27
    assert ray_div(4122 * 10**26, 1 * 10**27) == 4122 * 10**26
    assert ray_div(8745 * 10**24, 67 * 10**25) == 13052238805970149253731343284
    assert ray_div(6 * 10**27, 2 * 10**27) == 3 * 10**27


def test_wad_to_ray() -> None:
    assert wad_to_ray(1 * 10**18) == 1 * 10**27
    assert wad_to_ray(4122 * 10**17) == 4122 * 10**26
    assert wad_to_ray(0) == 0


def test_ray_to_wad() -> None:
    assert ray_to_wad(1 * 10**27) == 1 * 10**18
    assert ray_to_wad(4122 * 10**26) == 4122 * 10**17
    assert ray_to_wad(0) == 0


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


def test_ray_to_wad_half_up_rounding() -> None:
    half_ratio = WAD_RAY_RATIO // 2
    assert ray_to_wad(RAY + half_ratio) == WAD + 1
    assert ray_to_wad(RAY + half_ratio - 1) == WAD


def test_ray_to_wad_large_values() -> None:
    large_ray = 1000 * RAY
    assert ray_to_wad(large_ray) == 1000 * WAD


def test_wad_to_ray_overflow() -> None:
    large_value = (MAX_UINT256 // WAD_RAY_RATIO) + 1
    with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
        wad_to_ray(large_value)


def test_conversion_round_trip() -> None:
    original = 12345 * RAY
    assert wad_to_ray(ray_to_wad(original)) == original


@given(
    a=integers(min_value=0, max_value=MAX_UINT256), b=integers(min_value=0, max_value=MAX_UINT256)
)
@settings(max_examples=1000)
def test_wad_mul_fuzzing(a: int, b: int) -> None:
    if b == 0:
        expected = ((a * b) + HALF_WAD) // WAD
        assert wad_mul(a, b) == expected
    elif a > (MAX_UINT256 - HALF_WAD) // b:
        with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
            wad_mul(a, b)
    else:
        expected = ((a * b) + HALF_WAD) // WAD
        assert wad_mul(a, b) == expected


@given(
    a=integers(min_value=0, max_value=MAX_UINT256), b=integers(min_value=0, max_value=MAX_UINT256)
)
@settings(max_examples=1000)
def test_wad_div_fuzzing(a: int, b: int) -> None:
    if b == 0:
        with pytest.raises(EVMRevertError, match="ZERO_DIVISION"):
            wad_div(a, b)
        return

    overflow_threshold = (MAX_UINT256 - (b // 2)) // WAD
    if a > overflow_threshold:
        with pytest.raises(EVMRevertError, match="DIV_INTERNAL"):
            wad_div(a, b)
        return

    expected = ((a * WAD) + (b // 2)) // b
    assert wad_div(a, b) == expected


@given(a=integers(min_value=0, max_value=MAX_UINT256))
@settings(max_examples=1000)
def test_wad_to_ray_fuzzing(a: int) -> None:
    if a > MAX_UINT256 // WAD_RAY_RATIO:
        with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
            wad_to_ray(a)
    else:
        b = a * WAD_RAY_RATIO
        safety_check = b // WAD_RAY_RATIO == a

        if not safety_check:
            with pytest.raises(EVMRevertError, match="MUL_OVERFLOW"):
                wad_to_ray(a)
        else:
            expected = a * WAD_RAY_RATIO
            assert wad_to_ray(a) == expected
            assert wad_to_ray(a) == b


@given(a=integers(min_value=0, max_value=MAX_UINT256))
@settings(max_examples=1000)
def test_ray_to_wad_fuzzing(a: int) -> None:
    b = a // WAD_RAY_RATIO
    remainder = a % WAD_RAY_RATIO
    round_half = remainder >= (WAD_RAY_RATIO // 2)

    if round_half:
        expected = (a // WAD_RAY_RATIO) + 1
        assert ray_to_wad(a) == expected
        assert ray_to_wad(a) == b + 1
    else:
        expected = a // WAD_RAY_RATIO
        assert ray_to_wad(a) == expected
        assert ray_to_wad(a) == b
