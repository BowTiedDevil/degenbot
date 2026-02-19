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


def ray_div_ceil(a: int, b: int) -> int:
    """
    Divides two ray, rounding UP to ensure result >= true value.
    This matches Solidity ceiling division behavior for burns.
    """
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > MAX_UINT256 // RAY:
        raise EVMRevertError(error="DIV_INTERNAL")
    return ((a * RAY) // b) + (((a * RAY) % b) != 0)


def ray_div_floor(a: int, b: int) -> int:
    """
    Divides two ray, rounding DOWN (floor).
    This matches Solidity floor division behavior for mints.
    """
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a > MAX_UINT256 // RAY:
        raise EVMRevertError(error="DIV_INTERNAL")
    return (a * RAY) // b


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
