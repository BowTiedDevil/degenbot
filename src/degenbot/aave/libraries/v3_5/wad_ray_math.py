from typing import overload

from degenbot.aave.libraries.v3_5.rounding import Rounding
from degenbot.aave.libraries.v3_5.types import Ray, Wad
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError

# Wad: decimal numbers with 18 digits of precision
WAD = 10**18
HALF_WAD = 5 * 10**17

# Ray: decimal numbers with 27 digits of precision
RAY = 10**27
HALF_RAY = 5 * 10**26

# Ratio to convert between wad and ray
WAD_RAY_RATIO = 10**9


def wad_mul(a: Wad, b: Wad) -> Wad:
    if b != 0 and a > (MAX_UINT256 - HALF_WAD) // b:
        raise EVMRevertError(error="MUL_OVERFLOW")
    return (a * b + HALF_WAD) // WAD


def wad_div(a: Wad, b: Wad) -> Wad:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > (MAX_UINT256 - (b // 2)) // WAD:
        raise EVMRevertError(error="DIV_INTERNAL")
    return (a * WAD + (b // 2)) // b


@overload
def ray_mul(a: Ray, b: Ray) -> Ray: ...


@overload
def ray_mul(a: Ray, b: Ray, rounding: Rounding) -> Ray: ...


def ray_mul(a: Ray, b: Ray, rounding: Rounding | None = None) -> Ray:
    if rounding is None:
        if b != 0:
            limit = (MAX_UINT256 - HALF_RAY) // b
            if a > limit:
                raise EVMRevertError(error="MUL_OVERFLOW")
        return (a * b + HALF_RAY) // RAY
    if rounding == Rounding.FLOOR:
        return ray_mul_floor(a, b)
    return ray_mul_ceil(a, b)


def ray_mul_floor(a: Ray, b: Ray) -> Ray:
    if b != 0 and a > MAX_UINT256 // b:
        raise EVMRevertError(error="MUL_OVERFLOW")
    return (a * b) // RAY


def ray_mul_ceil(a: Ray, b: Ray) -> Ray:
    if b != 0 and a > MAX_UINT256 // b:
        raise EVMRevertError(error="MUL_OVERFLOW")
    product = a * b
    return product // RAY + (1 if product % RAY != 0 else 0)


@overload
def ray_div(a: Ray, b: Ray) -> Ray: ...


@overload
def ray_div(a: Ray, b: Ray, rounding: Rounding) -> Ray: ...


def ray_div(a: Ray, b: Ray, rounding: Rounding | None = None) -> Ray:
    if rounding is None:
        if b == 0:
            raise EVMRevertError(error="ZERO_DIVISION")
        limit = (MAX_UINT256 - (b // 2)) // RAY
        if a > limit:
            raise EVMRevertError(error="DIV_INTERNAL")
        return (a * RAY + (b // 2)) // b
    if rounding == Rounding.FLOOR:
        return ray_div_floor(a, b)
    return ray_div_ceil(a, b)


def ray_div_ceil(a: Ray, b: Ray) -> Ray:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > MAX_UINT256 // RAY:
        raise EVMRevertError(error="DIV_INTERNAL")
    scaled = a * RAY
    return scaled // b + (1 if scaled % b != 0 else 0)


def ray_div_floor(a: Ray, b: Ray) -> Ray:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > MAX_UINT256 // RAY:
        raise EVMRevertError(error="DIV_INTERNAL")
    return (a * RAY) // b


def ray_to_wad(a: Ray) -> Wad:
    result = a // WAD_RAY_RATIO
    remainder = a % WAD_RAY_RATIO
    if remainder >= (WAD_RAY_RATIO // 2):
        result += 1
    return result


def wad_to_ray(a: Wad) -> Ray:
    if a > MAX_UINT256 // WAD_RAY_RATIO:
        raise EVMRevertError(error="MUL_OVERFLOW")
    result = a * WAD_RAY_RATIO
    if result // WAD_RAY_RATIO != a:
        raise EVMRevertError(error="MUL_OVERFLOW")
    return result
