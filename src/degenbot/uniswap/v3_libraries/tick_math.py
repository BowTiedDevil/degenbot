import functools

from degenbot.constants import MAX_UINT128, MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.types.aliases import Tick
from degenbot.uniswap.v3_libraries._config import V3_LIB_CACHE_SIZE

MIN_TICK = -887272
MAX_TICK = -MIN_TICK
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342


# Magic number represents the minimum value of the error when approximating log_sqrt10001(x),
# when sqrtPrice is from the range (2^-64, 2^64). This is safe as MIN_SQRT_PRICE is more than
# 2^-64. If MIN_SQRT_PRICE is changed, this may need to be changed too
MIN_ERROR = 291339464771989622907027621153398088495

# Magic number represents the ceiling of the maximum value of the error when
# approximating log_sqrt10001(x)
MAX_ERROR = 3402992956809132418596140100660247210


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_sqrt_ratio_at_tick(tick: int) -> int:
    """
    Find the square root ratio in Q128.96 form for the given tick.
    """

    abs_tick = abs(tick)
    if not (abs_tick <= MAX_TICK):
        raise EVMRevertError(error="required: abs_tick <= MAX_TICK")

    ratio = 340265354078544963557816517032075149313 if abs_tick & 0x1 != 0 else MAX_UINT128 + 1

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
            ratio = (ratio * ratio_multiplier) >> 128

    if tick > 0:
        ratio = MAX_UINT256 // ratio

    # Divide by 1<<32, rounding up, to go from a Q128.128 to a Q128.96. Then downcast because the
    # result always fits within 160 bits due to tick input constraint. We round up in the division
    # so getTickAtSqrtRatio of the output price is always consistent.
    return (ratio >> 32) + (0 if (ratio % (1 << 32) == 0) else 1)


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_tick_at_sqrt_ratio(
    sqrt_price_x96: int,
) -> Tick:
    """
    Calculates the greatest tick value such that get_tick_at_sqrt_ratio(tick) <= ratio
    """

    if not (sqrt_price_x96 >= MIN_SQRT_RATIO and sqrt_price_x96 < MAX_SQRT_RATIO):
        msg = "R"
        raise EVMRevertError(msg)

    ratio = sqrt_price_x96 << 32

    r = ratio
    msb = 0
    for shift, factor in (
        (7, 340282366920938463463374607431768211455),
        (6, 18446744073709551615),
        (5, 4294967295),
        (4, 65535),
        (3, 255),
        (2, 15),
        (1, 3),
    ):
        f = (r > factor) << shift
        msb |= f
        r >>= f

    f = r > 1
    msb |= f
    r = ratio >> msb - 127 if msb >= 128 else ratio << 127 - msb  # noqa: PLR2004

    log_2 = (msb - 128) << 64

    for factor in (63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51):
        r = (r * r) >> 127
        f = r >> 128
        log_2 |= f << factor
        r >>= f

    r = (r * r) >> 127
    f = r >> 128
    log_2 |= f << 50

    log_sqrt10001 = log_2 * 255738958999603826347141  # 128.128 number

    tick_low = (log_sqrt10001 - MAX_ERROR) >> 128
    tick_high = (log_sqrt10001 + MIN_ERROR) >> 128

    return (
        tick_low
        if tick_low == tick_high
        else (tick_high if get_sqrt_ratio_at_tick(tick_high) <= sqrt_price_x96 else tick_low)
    )
