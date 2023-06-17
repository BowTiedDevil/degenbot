from degenbot.exceptions import EVMRevertError

# adapted from OpenZeppelin's overflow checks, which throw
# an exception if the input value exceeds the maximum value
# for this type

MIN_INT16 = -(2**15)
MAX_INT16 = 2**15 - 1

MIN_INT128 = -(2**127)
MAX_INT128 = 2**127 - 1

MAX_UINT8 = 0
MAX_UINT8 = 2**8 - 1

MIN_UINT128 = 0
MAX_UINT128 = 2**128 - 1

MIN_UINT160 = 0
MAX_UINT160 = 2**160 - 1

MIN_UINT256 = 0
MAX_UINT256 = 2**256 - 1


def mulmod(x, y, k):
    if k == 0:
        raise EVMRevertError
    return (x * y) % k


def to_int128(x):
    if not (x <= 2 ** (128 - 1)):
        raise EVMRevertError
    return x


def to_int256(x):
    if not (x <= 2 ** (256 - 1)):
        raise EVMRevertError
    return x


def to_uint160(x):
    if not (x <= 2 ** (160) - 1):
        raise EVMRevertError
    return x


# Dumb integer "conversion" that performs no value checking to mimic Solidity's
# inline typecasting for int/uint types. Makes copy-pasting the Solidity functions
# easier since in-line casts can remain
for i in range(8, 256 + 8, 8):
    exec(f"def int{i}(x): return x")
    exec(f"def uint{i}(x): return x")
