from ...constants import (
    MAX_INT128,
    MAX_INT256,
    MAX_UINT160,
    MIN_INT128,
    MIN_INT256,
)
from ...exceptions import EVMRevertError


def mulmod(x, y, k):
    if k == 0:
        raise EVMRevertError
    return (x * y) % k


# adapted from OpenZeppelin's overflow checks, which throw
# an exception if the input value exceeds the maximum value
# for this type
def to_int128(x):
    if not (MIN_INT128 <= x <= MAX_INT128):
        raise EVMRevertError(f"{x} outside range of int128 values")
    return x


def to_int256(x):
    if not (MIN_INT256 <= x <= MAX_INT256):
        raise EVMRevertError(f"{x} outside range of int256 values")
    return x


def to_uint160(x):
    if x > MAX_UINT160:
        raise EVMRevertError(f"{x} greater than maximum uint160 value")
    return x


# Dumb integer "conversions" that performs no value checking to mimic Solidity's
# inline typecasting for int/uint types. Makes copy-pasting the Solidity functions
# easier since in-line casts can remain
def int8(x):
    return x


def int16(x):
    return x


def int24(x):
    return x


def int32(x):
    return x


def int40(x):
    return x


def int48(x):
    return x


def int56(x):
    return x


def int64(x):
    return x


def int72(x):
    return x


def int80(x):
    return x


def int88(x):
    return x


def int96(x):
    return x


def int104(x):
    return x


def int112(x):
    return x


def int120(x):
    return x


def int128(x):
    return x


def int136(x):
    return x


def int144(x):
    return x


def int152(x):
    return x


def int160(x):
    return x


def int168(x):
    return x


def int176(x):
    return x


def int184(x):
    return x


def int192(x):
    return x


def int200(x):
    return x


def int208(x):
    return x


def int216(x):
    return x


def int224(x):
    return x


def int232(x):
    return x


def int240(x):
    return x


def int248(x):
    return x


def int256(x):
    return x


def uint8(x):
    return x


def uint16(x):
    return x


def uint24(x):
    return x


def uint32(x):
    return x


def uint40(x):
    return x


def uint48(x):
    return x


def uint56(x):
    return x


def uint64(x):
    return x


def uint72(x):
    return x


def uint80(x):
    return x


def uint88(x):
    return x


def uint96(x):
    return x


def uint104(x):
    return x


def uint112(x):
    return x


def uint120(x):
    return x


def uint128(x):
    return x


def uint136(x):
    return x


def uint144(x):
    return x


def uint152(x):
    return x


def uint160(x):
    return x


def uint168(x):
    return x


def uint176(x):
    return x


def uint184(x):
    return x


def uint192(x):
    return x


def uint200(x):
    return x


def uint208(x):
    return x


def uint216(x):
    return x


def uint224(x):
    return x


def uint232(x):
    return x


def uint240(x):
    return x


def uint248(x):
    return x


def uint256(x):
    return x
