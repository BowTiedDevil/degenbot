import hypothesis
import hypothesis.strategies
import pytest

from degenbot.aave.libraries.wad_ray_math import (
    HALF_RAY,
    HALF_WAD,
    RAY,
    WAD,
    WAD_RAY_RATIO,
    ray_div,
    ray_mul,
    ray_to_wad,
    wad_div,
    wad_mul,
    wad_to_ray,
)
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions.evm import EVMRevertError


def test_constants() -> None:
    assert WAD == 1 * 10**18
    assert HALF_WAD == 1 * 10**18 // 2
    assert RAY == 1 * 10**27
    assert HALF_RAY == 1 * 10**27 // 2


def test_wad_mul_edge() -> None:
    assert wad_mul(0, WAD) == 0
    assert wad_mul(WAD, 0) == 0
    assert wad_mul(0, 0) == 0


@hypothesis.given(
    a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    b=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_wad_mul_fuzzing(a: int, b: int) -> None:
    if (b == 0 or (a > (MAX_UINT256 - HALF_WAD) // b) is False) is False:
        with pytest.raises(EVMRevertError):
            wad_mul(a, b)
    else:
        assert wad_mul(a, b) == (a * b + HALF_WAD) // WAD


@hypothesis.given(
    a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
    b=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256),
)
def test_wad_div_fuzzing(a: int, b: int) -> None:

    if (b == 0) or (((a > ((MAX_UINT256 - b // 2) / WAD)) is False) is False):
        with pytest.raises(EVMRevertError):
            wad_div(a, b)
    else:
        assert wad_div(a, b) == (a * WAD + (b // 2)) // b


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


@hypothesis.given(a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256))
def test_wad_to_ray_fuzzing(a: int) -> None:
    """
    Foundry test function `test_wadToRay_fuzz` includes an overflow check:
        b = a * w.WAD_RAY_RATIO();
        safetyCheck = b / w.WAD_RAY_RATIO() == a;

    Python integers cannot overflow, so the check has been replaced by a direct check against the
    uint256 upper boundary.
    """

    if a * WAD_RAY_RATIO > MAX_UINT256:
        with pytest.raises(EVMRevertError):
            wad_to_ray(a)
    else:
        assert wad_to_ray(a) == a * WAD_RAY_RATIO


@hypothesis.given(a=hypothesis.strategies.integers(min_value=MIN_UINT256, max_value=MAX_UINT256))
def test_ray_to_wad_fuzzing(a: int) -> None:
    """
    Check that `ray_to_wad` does not round the result up if remainder is less than half the wad to
    ray ratio.
    """

    assert ray_to_wad(a) == a // WAD_RAY_RATIO + ((a % WAD_RAY_RATIO) >= (WAD_RAY_RATIO // 2))
