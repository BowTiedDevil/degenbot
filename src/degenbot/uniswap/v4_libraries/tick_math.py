import functools

from degenbot.constants import MAX_INT16, MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.functions import evm_divide
from degenbot.uniswap.v4_libraries import bit_math
from degenbot.uniswap.v4_libraries._config import V4_LIB_CACHE_SIZE

MIN_TICK = -887272
MAX_TICK = 887272
MIN_TICK_SPACING = 1
MAX_TICK_SPACING = MAX_INT16
MIN_SQRT_PRICE = 4295128739
MAX_SQRT_PRICE = 1461446703485210103287273052203988822378723970342
MAX_SQRT_PRICE_MINUS_MIN_SQRT_PRICE_MINUS_ONE = (
    1461446703485210103287273052203988822378723970342 - 4295128739 - 1
)


# Magic number represents the minimum value of the error when approximating log_sqrt10001(x),
# when sqrtPrice is from the range (2^-64, 2^64). This is safe as MIN_SQRT_PRICE is more than
# 2^-64. If MIN_SQRT_PRICE is changed, this may need to be changed too
MIN_ERROR = 291339464771989622907027621153398088495

# Magic number represents the ceiling of the maximum value of the error when
# approximating log_sqrt10001(x)
MAX_ERROR = 3402992956809132418596140100660247210


def max_usable_tick(tick_spacing: int) -> int:
    """
    Given a tickSpacing, compute the maximum usable tick
    """

    return (MAX_TICK // tick_spacing) * tick_spacing


def min_usable_tick(tick_spacing: int) -> int:
    """
    Given a tickSpacing, compute the minimum usable tick
    """

    return evm_divide(MIN_TICK, tick_spacing) * tick_spacing


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_sqrt_price_at_tick(tick: int) -> int:
    """
    Calculates sqrt(1.0001^tick) * 2^96, a fixed point Q64.96 number representing the sqrt of the
    price of the two assets (currency1/currency0) at the given tick.

    Raises exception if |tick| > max tick.
    """

    # Use abs instead of reimplementing the Solidity contract's inline Yul
    abs_tick = abs(tick)
    if abs_tick > MAX_TICK:
        msg = "InvalidTick"
        raise EVMRevertError(msg)

    # The tick is decomposed into bits, and for each bit with index i that is set, the product of
    # 1/sqrt(1.0001^(2^i)) is calculated (using Q128.128). The constants used for this calculation
    # are rounded to the nearest integer

    # Equivalent to:
    #     price = absTick & 0x1 != 0 ?
    #         0xfffcb933bd6fad37aa2d162d1a594001 :
    #         0x100000000000000000000000000000000;
    #     or price = int(2**128 / sqrt(1.0001)) if (absTick & 0x1) else 1 << 128

    price = (1 << 128) ^ (((1 << 128) ^ 340265354078544963557816517032075149313) * (abs_tick & 1))

    for tick_mask, ratio_multiplier in (
        (2, 340248342086729790484326174814286782778),
        (4, 340214320654664324051920982716015181260),
        (8, 340146287995602323631171512101879684304),
        (16, 340010263488231146823593991679159461444),
        (32, 339738377640345403697157401104375502016),
        (64, 339195258003219555707034227454543997025),
        (128, 338111622100601834656805679988414885971),
        (256, 335954724994790223023589805789778977700),
        (512, 331682121138379247127172139078559817300),
        (1024, 323299236684853023288211250268160618739),
        (2048, 307163716377032989948697243942600083929),
        (4096, 277268403626896220162999269216087595045),
        (8192, 225923453940442621947126027127485391333),
        (16384, 149997214084966997727330242082538205943),
        (32768, 66119101136024775622716233608466517926),
        (65536, 12847376061809297530290974190478138313),
        (131072, 485053260817066172746253684029974020),
        (262144, 691415978906521570653435304214168),
        (524288, 1404880482679654955896180642),
    ):
        if abs_tick & tick_mask != 0:
            price = (price * ratio_multiplier) >> 128

    if tick > 0:
        price = MAX_UINT256 // price

    # This divides by 1<<32 rounding up to go from a Q128.128 to a Q128.96.
    # Then downcast because the result always fits within 160 bits due to our tick input constraint.
    # Round up in the division so getTickAtSqrtPrice of the output price is always consistent
    # `sub(shl(32, 1), 1)` is `type(uint32).max`
    # `price + type(uint32).max` will not overflow because `price` fits in 192 bits
    return (price + ((1 << 32) - 1)) >> 32


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_tick_at_sqrt_price(sqrt_price_x96: int) -> int:
    """
    Calculates the greatest tick value such that getSqrtPriceAtTick(tick) <= sqrtPriceX96

    @dev raises exception if sqrt_price_x96 is below MIN_SQRT_PRICE or above MAX_SQRT_PRICE.
    """

    if sqrt_price_x96 < MIN_SQRT_PRICE or sqrt_price_x96 > MAX_SQRT_PRICE:
        msg = "InvalidSqrtPrice"
        raise EVMRevertError(msg)

    price = sqrt_price_x96 << 32
    msb = bit_math.most_significant_bit(price)
    r = price >> msb - 127 if msb >= 128 else price << 127 - msb  # noqa: PLR2004
    log_2 = (msb - 128) << 64

    for factor in (63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51):
        r = (r * r) >> 127
        f = r >> 128
        log_2 |= f << factor
        r >>= f

    r = (r * r) >> 127
    f = r >> 128
    log_2 |= f << 50

    log_sqrt10001 = log_2 * 255738958999603826347141  # Q22.128 number

    tick_low = (log_sqrt10001 - MAX_ERROR) >> 128
    tick_high = (log_sqrt10001 + MIN_ERROR) >> 128

    return (
        tick_low
        if tick_low == tick_high
        else (tick_high if get_sqrt_price_at_tick(tick_high) <= sqrt_price_x96 else tick_low)
    )
