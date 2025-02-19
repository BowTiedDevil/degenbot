from degenbot.constants import MAX_INT128, MAX_INT256, MAX_UINT160, MIN_INT128, MIN_INT256
from degenbot.exceptions import EVMRevertError


def mulmod(x: int, y: int, k: int) -> int:
    if k == 0:
        raise EVMRevertError(error="division by zero")
    return (x * y) % k


# adapted from OpenZeppelin's overflow checks, which throw
# an exception if the input value exceeds the maximum value
# for this type
def to_int128(x: int) -> int:
    if not (MIN_INT128 <= x <= MAX_INT128):
        raise EVMRevertError(error=f"{x} outside range of int128 values")
    return x


def to_int256(x: int) -> int:
    if not (MIN_INT256 <= x <= MAX_INT256):
        raise EVMRevertError(error=f"{x} outside range of int256 values")
    return x


def to_uint160(x: int) -> int:
    if x > MAX_UINT160:
        raise EVMRevertError(error=f"{x} greater than maximum uint160 value")
    return x
