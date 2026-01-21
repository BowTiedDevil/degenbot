from degenbot.aave.libraries.v3_4.constants import HALF_RAY, HALF_WAD, RAY, WAD, WAD_RAY_RATIO
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError


def wad_mul(a: int, b: int) -> int:
    if b != 0 and a > (MAX_UINT256 - HALF_WAD) // b:
        raise EVMRevertError(error="MUL_OVERFLOW")
    return (a * b + HALF_WAD) // WAD


def wad_div(a: int, b: int) -> int:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > (MAX_UINT256 - (b // 2)) // WAD:
        raise EVMRevertError(error="DIV_INTERNAL")
    return (a * WAD + (b // 2)) // b


def ray_mul(a: int, b: int) -> int:
    if b != 0:
        limit = (MAX_UINT256 - HALF_RAY) // b
        if a > limit:
            raise EVMRevertError(error="MUL_OVERFLOW")
    return (a * b + HALF_RAY) // RAY


def ray_div(a: int, b: int) -> int:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    limit = (MAX_UINT256 - (b // 2)) // RAY
    if a > limit:
        raise EVMRevertError(error="DIV_INTERNAL")
    return (a * RAY + (b // 2)) // b


def ray_to_wad(a: int) -> int:
    result = a // WAD_RAY_RATIO
    remainder = a % WAD_RAY_RATIO
    if remainder >= (WAD_RAY_RATIO // 2):
        result += 1
    return result


def wad_to_ray(a: int) -> int:
    if a > MAX_UINT256 // WAD_RAY_RATIO:
        raise EVMRevertError(error="MUL_OVERFLOW")
    result = a * WAD_RAY_RATIO
    if result // WAD_RAY_RATIO != a:
        raise EVMRevertError(error="MUL_OVERFLOW")
    return result
