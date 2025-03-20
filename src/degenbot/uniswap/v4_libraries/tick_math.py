# ruff: noqa: PLR2004

from typing import Annotated

from pydantic import Field, validate_call

from degenbot.constants import MAX_INT16, MAX_UINT256
from degenbot.uniswap.v4_libraries import bit_math
from degenbot.validation.evm_values import ValidatedInt24, ValidatedUint160

MIN_TICK = -887272
MAX_TICK = 887272
MIN_TICK_SPACING = 1
MAX_TICK_SPACING = MAX_INT16
MIN_SQRT_PRICE = 4295128739
MAX_SQRT_PRICE = 1461446703485210103287273052203988822378723970342
MAX_SQRT_PRICE_MINUS_MIN_SQRT_PRICE_MINUS_ONE = (
    1461446703485210103287273052203988822378723970342 - 4295128739 - 1
)

type ValidatedTick = Annotated[int, Field(strict=True, ge=MIN_TICK, le=MAX_TICK)]
type ValidatedSqrtPrice = Annotated[int, Field(strict=True, ge=MIN_SQRT_PRICE, le=MAX_SQRT_PRICE)]


@validate_call(validate_return=True)
def max_usable_tick(tick_spacing: ValidatedInt24) -> ValidatedInt24:
    """
    Given a tickSpacing, compute the maximum usable tick
    """

    return (MAX_TICK // tick_spacing) * tick_spacing


@validate_call(validate_return=True)
def min_usable_tick(tick_spacing: ValidatedInt24) -> ValidatedInt24:
    """
    Given a tickSpacing, compute the minimum usable tick
    """

    return (MIN_TICK // tick_spacing) * tick_spacing


@validate_call(validate_return=True)
def get_sqrt_price_at_tick(tick: ValidatedTick) -> ValidatedUint160:
    """
    Calculates sqrt(1.0001^tick) * 2^96, a fixed point Q64.96 number representing the sqrt of the
    price of the two assets (currency1/currency0) at the given tick.

    Raises exception if |tick| > max tick.
    """

    # Use abs instead of reimplementing the Solidity contract's inline Yul
    abs_tick = abs(tick)

    # The tick is decomposed into bits, and for each bit with index i that is set, the product of
    # 1/sqrt(1.0001^(2^i)) is calculated (using Q128.128). The constants used for this calculation
    # are rounded to the nearest integer

    # Equivalent to:
    #     price = absTick & 0x1 != 0 ?
    #         0xfffcb933bd6fad37aa2d162d1a594001 :
    #         0x100000000000000000000000000000000;
    #     or price = int(2**128 / sqrt(1.0001)) if (absTick & 0x1) else 1 << 128

    price = (1 << 128) ^ (((1 << 128) ^ 0xFFFCB933BD6FAD37AA2D162D1A594001) * (abs_tick & 0x1))

    if abs_tick & 0x2 != 0:
        price = (price * 0xFFF97272373D413259A46990580E213A) >> 128
    if abs_tick & 0x4 != 0:
        price = (price * 0xFFF2E50F5F656932EF12357CF3C7FDCC) >> 128
    if abs_tick & 0x8 != 0:
        price = (price * 0xFFE5CACA7E10E4E61C3624EAA0941CD0) >> 128
    if abs_tick & 0x10 != 0:
        price = (price * 0xFFCB9843D60F6159C9DB58835C926644) >> 128
    if abs_tick & 0x20 != 0:
        price = (price * 0xFF973B41FA98C081472E6896DFB254C0) >> 128
    if abs_tick & 0x40 != 0:
        price = (price * 0xFF2EA16466C96A3843EC78B326B52861) >> 128
    if abs_tick & 0x80 != 0:
        price = (price * 0xFE5DEE046A99A2A811C461F1969C3053) >> 128
    if abs_tick & 0x100 != 0:
        price = (price * 0xFCBE86C7900A88AEDCFFC83B479AA3A4) >> 128
    if abs_tick & 0x200 != 0:
        price = (price * 0xF987A7253AC413176F2B074CF7815E54) >> 128
    if abs_tick & 0x400 != 0:
        price = (price * 0xF3392B0822B70005940C7A398E4B70F3) >> 128
    if abs_tick & 0x800 != 0:
        price = (price * 0xE7159475A2C29B7443B29C7FA6E889D9) >> 128
    if abs_tick & 0x1000 != 0:
        price = (price * 0xD097F3BDFD2022B8845AD8F792AA5825) >> 128
    if abs_tick & 0x2000 != 0:
        price = (price * 0xA9F746462D870FDF8A65DC1F90E061E5) >> 128
    if abs_tick & 0x4000 != 0:
        price = (price * 0x70D869A156D2A1B890BB3DF62BAF32F7) >> 128
    if abs_tick & 0x8000 != 0:
        price = (price * 0x31BE135F97D08FD981231505542FCFA6) >> 128
    if abs_tick & 0x10000 != 0:
        price = (price * 0x9AA508B5B7A84E1C677DE54F3E99BC9) >> 128
    if abs_tick & 0x20000 != 0:
        price = (price * 0x5D6AF8DEDB81196699C329225EE604) >> 128
    if abs_tick & 0x40000 != 0:
        price = (price * 0x2216E584F5FA1EA926041BEDFE98) >> 128
    if abs_tick & 0x80000 != 0:
        price = (price * 0x48A170391F7DC42444E8FA2) >> 128

    if tick > 0:
        price = MAX_UINT256 // price

    # This divides by 1<<32 rounding up to go from a Q128.128 to a Q128.96.
    # Then downcast because the result always fits within 160 bits due to our tick input constraint.
    # Round up in the division so getTickAtSqrtPrice of the output price is always consistent
    # `sub(shl(32, 1), 1)` is `type(uint32).max`
    # `price + type(uint32).max` will not overflow because `price` fits in 192 bits
    return (price + ((1 << 32) - 1)) >> 32


@validate_call(validate_return=True)
def get_tick_at_sqrt_price(sqrt_price_x96: ValidatedSqrtPrice) -> ValidatedTick:
    """
    Calculates the greatest tick value such that getSqrtPriceAtTick(tick) <= sqrtPriceX96

    @dev raises exception if sqrt_price_x96 is below MIN_SQRT_PRICE or above MAX_SQRT_PRICE.
    """

    price = sqrt_price_x96 << 32
    r = price
    msb = bit_math.most_significant_bit(r)
    r = price >> msb - 127 if msb >= 128 else price << 127 - msb

    log_2 = (msb - 128) << 64
    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 63)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 62)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 61)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 60)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 59)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 58)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 57)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 56)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 55)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 54)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 53)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 52)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 51)
    r = r >> f

    r = (r * r) >> 127
    f = r >> 128
    log_2 = log_2 | (f << 50)

    log_sqrt10001 = log_2 * 255738958999603826347141  # Q22.128 number

    # Magic number represents the ceiling of the maximum value of the error when
    # approximating log_sqrt10001(x)
    tick_low = (log_sqrt10001 - 3402992956809132418596140100660247210) >> 128
    # Magic number represents the minimum value of the error when approximating log_sqrt10001(x),
    # when sqrtPrice is from the range (2^-64, 2^64). This is safe as MIN_SQRT_PRICE is more than
    # 2^-64. If MIN_SQRT_PRICE is changed, this may need to be changed too
    tick_hi = (log_sqrt10001 + 291339464771989622907027621153398088495) >> 128
    return (
        tick_low
        if tick_low == tick_hi
        else (tick_hi if get_sqrt_price_at_tick(tick_hi) <= sqrt_price_x96 else tick_low)
    )
