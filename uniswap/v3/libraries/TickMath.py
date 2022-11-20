from .Helpers import int24, int256, uint160, uint256
from . import YulOperations as yul

MIN_TICK = -887272
MAX_TICK = -MIN_TICK
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342


def getSqrtRatioAtTick(tick: int) -> int:

    absTick = uint256(-int256(tick)) if tick < 0 else uint256(int256(tick))
    assert absTick <= uint256(MAX_TICK), "T"

    ratio = (
        0xFFFCB933BD6FAD37AA2D162D1A594001
        if (absTick & 0x1 != 0)
        else 0x100000000000000000000000000000000
    )

    if absTick & 0x2 != 0:
        ratio = (ratio * 0xFFF97272373D413259A46990580E213A) >> 128
    if absTick & 0x4 != 0:
        ratio = (ratio * 0xFFF2E50F5F656932EF12357CF3C7FDCC) >> 128
    if absTick & 0x8 != 0:
        ratio = (ratio * 0xFFE5CACA7E10E4E61C3624EAA0941CD0) >> 128
    if absTick & 0x10 != 0:
        ratio = (ratio * 0xFFCB9843D60F6159C9DB58835C926644) >> 128
    if absTick & 0x20 != 0:
        ratio = (ratio * 0xFF973B41FA98C081472E6896DFB254C0) >> 128
    if absTick & 0x40 != 0:
        ratio = (ratio * 0xFF2EA16466C96A3843EC78B326B52861) >> 128
    if absTick & 0x80 != 0:
        ratio = (ratio * 0xFE5DEE046A99A2A811C461F1969C3053) >> 128
    if absTick & 0x100 != 0:
        ratio = (ratio * 0xFCBE86C7900A88AEDCFFC83B479AA3A4) >> 128
    if absTick & 0x200 != 0:
        ratio = (ratio * 0xF987A7253AC413176F2B074CF7815E54) >> 128
    if absTick & 0x400 != 0:
        ratio = (ratio * 0xF3392B0822B70005940C7A398E4B70F3) >> 128
    if absTick & 0x800 != 0:
        ratio = (ratio * 0xE7159475A2C29B7443B29C7FA6E889D9) >> 128
    if absTick & 0x1000 != 0:
        ratio = (ratio * 0xD097F3BDFD2022B8845AD8F792AA5825) >> 128
    if absTick & 0x2000 != 0:
        ratio = (ratio * 0xA9F746462D870FDF8A65DC1F90E061E5) >> 128
    if absTick & 0x4000 != 0:
        ratio = (ratio * 0x70D869A156D2A1B890BB3DF62BAF32F7) >> 128
    if absTick & 0x8000 != 0:
        ratio = (ratio * 0x31BE135F97D08FD981231505542FCFA6) >> 128
    if absTick & 0x10000 != 0:
        ratio = (ratio * 0x9AA508B5B7A84E1C677DE54F3E99BC9) >> 128
    if absTick & 0x20000 != 0:
        ratio = (ratio * 0x5D6AF8DEDB81196699C329225EE604) >> 128
    if absTick & 0x40000 != 0:
        ratio = (ratio * 0x2216E584F5FA1EA926041BEDFE98) >> 128
    if absTick & 0x80000 != 0:
        ratio = (ratio * 0x48A170391F7DC42444E8FA2) >> 128

    if tick > 0:
        ratio = (2**256 - 1) // ratio

    # this divides by 1<<32 rounding up to go from a Q128.128 to a Q128.96
    # we then downcast because we know the result always fits within 160 bits due to our tick input constraint
    # we round up in the division so getTickAtSqrtRatio of the output price is always consistent
    return uint160((ratio >> 32) + (0 if (ratio % (1 << 32) == 0) else 1))


def getTickAtSqrtRatio(sqrtPriceX96: int) -> int:

    # second inequality must be < because the price can never reach the price at the max tick
    assert (
        sqrtPriceX96 >= MIN_SQRT_RATIO and sqrtPriceX96 < MAX_SQRT_RATIO
    ), "R"
    ratio = uint256(sqrtPriceX96) << 32

    r: int = ratio
    msb: int = 0

    f = yul.shl(7, yul.gt(r, 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(7, yul.gt(r, 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(6, yul.gt(r, 0xFFFFFFFFFFFFFFFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(5, yul.gt(r, 0xFFFFFFFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(4, yul.gt(r, 0xFFFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(3, yul.gt(r, 0xFF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(2, yul.gt(r, 0xF))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.shl(1, yul.gt(r, 0x3))
    msb = yul._or(msb, f)
    r = yul.shr(f, r)

    f = yul.gt(r, 0x1)
    msb = yul._or(msb, f)

    if msb >= 128:
        r = ratio >> (msb - 127)
    else:
        r = ratio << (127 - msb)

    log_2 = (int(msb) - 128) << 64

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(63, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(62, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(61, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(60, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(59, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(58, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(57, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(56, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(55, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(54, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(53, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(52, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(51, f))
    r = yul.shr(f, r)

    r = yul.shr(127, yul.mul(r, r))
    f = yul.shr(128, r)
    log_2 = yul._or(log_2, yul.shl(50, f))

    log_sqrt10001 = log_2 * 255738958999603826347141  # 128.128 number

    tickLow = int24(
        (log_sqrt10001 - 3402992956809132418596140100660247210) >> 128
    )
    tickHi = int24(
        (log_sqrt10001 + 291339464771989622907027621153398088495) >> 128
    )

    tick = (
        tickLow
        if (tickLow == tickHi)
        else (
            tickHi if getSqrtRatioAtTick(tickHi) <= sqrtPriceX96 else tickLow
        )
    )

    return tick
