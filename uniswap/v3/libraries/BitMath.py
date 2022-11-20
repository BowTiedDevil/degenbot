def mostSignificantBit(x: int) -> int:

    assert x > 0, "FAIL: x > 0"

    r = 0

    if x >= 0x100000000000000000000000000000000:
        x >>= 128
        r += 128

    if x >= 0x10000000000000000:
        x >>= 64
        r += 64

    if x >= 0x100000000:
        x >>= 32
        r += 32

    if x >= 0x10000:
        x >>= 16
        r += 16

    if x >= 0x100:
        x >>= 8
        r += 8

    if x >= 0x10:
        x >>= 4
        r += 4

    if x >= 0x4:
        x >>= 2
        r += 2

    if x >= 0x2:
        r += 1

    return r


def leastSignificantBit(x: int) -> int:

    assert x > 0, "FAIL: x > 0"

    r = 255
    if x & 2**128 - 1 > 0:
        r -= 128
    else:
        x >>= 128

    if x & 2**64 - 1 > 0:
        r -= 64
    else:
        x >>= 64

    if x & 2**32 - 1 > 0:
        r -= 32
    else:
        x >>= 32

    if x & 2**16 - 1 > 0:
        r -= 16
    else:
        x >>= 16

    if x & 2**8 - 1 > 0:
        r -= 8
    else:
        x >>= 8

    if x & 0xF > 0:
        r -= 4
    else:
        x >>= 4

    if x & 0x3 > 0:
        r -= 2
    else:
        x >>= 2

    if x & 0x1 > 0:
        r -= 1

    return r
