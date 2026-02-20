from enum import Enum

from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError

# Wad: decimal numbers with 18 digits of precision
WAD = 10**18
HALF_WAD = 5 * 10**17

# Ray: decimal numbers with 27 digits of precision
RAY = 10**27
HALF_RAY = 5 * 10**26

# Ratio to convert between Wad and Ray
WAD_RAY_RATIO = 10**9


class Rounding(Enum):
    FLOOR = 0
    CEIL = 1


def _raise_on_overflow(value: int) -> None:
    if value > MAX_UINT256:
        raise EVMRevertError


def _raise_on_zero_division(divisor: int) -> None:
    if divisor == 0:
        raise EVMRevertError


def wad_mul(a: int, b: int) -> int:
    """
    Multiplies two wad, rounding half up to the nearest wad.
    """

    if a * b + HALF_WAD > MAX_UINT256:
        raise EVMRevertError
    return (a * b + HALF_WAD) // WAD


def wad_div(a: int, b: int) -> int:
    """
    Divides two wad, rounding half up to the nearest wad.
    """

    _raise_on_overflow(a * WAD + b // 2)
    _raise_on_zero_division(b)
    return (a * WAD + b // 2) // b


def ray_mul(a: int, b: int, rounding: Rounding | None = None) -> int:
    match rounding:
        case None:
            _raise_on_overflow(a * b + HALF_RAY)
            return (a * b + HALF_RAY) // RAY
        case Rounding.FLOOR:
            return ray_mul_floor(a, b)
        case Rounding.CEIL:
            return ray_mul_ceil(a, b)


def ray_mul_floor(a: int, b: int) -> int:
    _raise_on_overflow(a * b)
    return (a * b) // RAY


def ray_mul_ceil(a: int, b: int) -> int:
    _raise_on_overflow(a * b)
    return ((a * b) // RAY) + ((a * b) % RAY != 0)


def ray_div(a: int, b: int, rounding: Rounding | None = None) -> int:
    """
    Divides two ray, rounding half up to the nearest ray if a specific rounding mode is not
    specified.
    """

    _raise_on_zero_division(b)

    match rounding:
        case None:
            if ((a * RAY) + (b // 2)) > MAX_UINT256:
                raise EVMRevertError
            return ((a * RAY) + (b // 2)) // b
        case Rounding.FLOOR:
            return ray_div_floor(a, b)
        case Rounding.CEIL:
            return ray_div_ceil(a, b)


def ray_div_ceil(a: int, b: int) -> int:
    _raise_on_overflow(a * RAY)
    _raise_on_zero_division(b)
    return ((a * RAY) // b) + (((a * RAY) % b) != 0)


def ray_div_floor(a: int, b: int) -> int:
    _raise_on_overflow(a * RAY)
    _raise_on_zero_division(b)
    return (a * RAY) // b


def ray_to_wad(a: int) -> int:
    """
    Casts ray value down to wad, rounding half up to the nearest wad.
    """

    return (a // WAD_RAY_RATIO) + (a % WAD_RAY_RATIO > WAD_RAY_RATIO // 2)


def wad_to_ray(a: int) -> int:
    """
    Convert wad value up to ray.
    """

    _raise_on_overflow(a * WAD_RAY_RATIO)
    return a * WAD_RAY_RATIO
