"""
Port of UniswapV3 TickMath.sol.

Source: contracts/libraries/TickMath.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/TickMath.sol

Computes sqrt prices from ticks and vice versa, where tick values correspond
to sqrt(1.0001^tick) as Q64.96 fixed-point numbers.

The original Solidity uses inline assembly for binary search (MSB) and
iterative squaring (log₂). This port reimplements those algorithms in
pure Vyper using shift/comparison operations.
"""

# Tick bounds
MIN_TICK: constant(int24) = -887272
MAX_TICK: constant(int24) = 887272

# Sqrt price bounds (from getSqrtRatioAtTick at min/max tick)
MIN_SQRT_RATIO: constant(uint160) = 4295128739
MAX_SQRT_RATIO: constant(uint160) = 1461446703485210103287273052203988822378723970342

# Magic constants for getSqrtRatioAtTick (precomputed powers of sqrt(1.0001^2^i))
# Each constant is sqrt(1.0001^(2^i)) * 2^128, for i = 0..18
M0: constant(uint256) = 340265354078544963557816517032075149313
M1: constant(uint256) = 340248342086729790484326174814286782778
M2: constant(uint256) = 340214320654664324051920982716015181260
M3: constant(uint256) = 340146287995602323631171512101879684304
M4: constant(uint256) = 340010263488231146823593991679159461444
M5: constant(uint256) = 339738377640345403697157401104375502016
M6: constant(uint256) = 339195258003219555707034227454543997025
M7: constant(uint256) = 338111622100601834656805679988414885971
M8: constant(uint256) = 335954724994790223023589805789778977700
M9: constant(uint256) = 331682121138379247127172139078559817300
M10: constant(uint256) = 323299236684853023288211250268160618739
M11: constant(uint256) = 307163716377032989948697243942600083929
M12: constant(uint256) = 277268403626896220162999269216087595045
M13: constant(uint256) = 225923453940442621947126027127485391333
M14: constant(uint256) = 149997214084966997727330242082538205943
M15: constant(uint256) = 66119101136024775622716233608466517926
M16: constant(uint256) = 12847376061809297530290974190478138313
M17: constant(uint256) = 485053260817066172746253684029974020
M18: constant(uint256) = 691415978906521570653435304214168
M19: constant(uint256) = 1404880482679654955896180642


@internal
@pure
def get_sqrt_ratio_at_tick(tick: int24) -> uint160:
    """Calculates sqrt(1.0001^tick) * 2^96.

    Throws if |tick| > max tick.
    """
    abs_tick: uint256 = empty(uint256)
    if tick >= 0:
        abs_tick = convert(tick, uint256)
    else:
        abs_tick = convert(-convert(tick, int256), uint256)
    assert abs_tick <= convert(MAX_TICK, uint256), "T"

    ratio: uint256 = 340282366920938463463374607431768211456  # 2^128
    if (abs_tick & 1) != 0:
        ratio = M0
    if (abs_tick & 2) != 0:
        ratio = unsafe_mul(ratio, M1) >> 128
    if (abs_tick & 4) != 0:
        ratio = unsafe_mul(ratio, M2) >> 128
    if (abs_tick & 8) != 0:
        ratio = unsafe_mul(ratio, M3) >> 128
    if (abs_tick & 16) != 0:
        ratio = unsafe_mul(ratio, M4) >> 128
    if (abs_tick & 32) != 0:
        ratio = unsafe_mul(ratio, M5) >> 128
    if (abs_tick & 64) != 0:
        ratio = unsafe_mul(ratio, M6) >> 128
    if (abs_tick & 128) != 0:
        ratio = unsafe_mul(ratio, M7) >> 128
    if (abs_tick & 256) != 0:
        ratio = unsafe_mul(ratio, M8) >> 128
    if (abs_tick & 512) != 0:
        ratio = unsafe_mul(ratio, M9) >> 128
    if (abs_tick & 1024) != 0:
        ratio = unsafe_mul(ratio, M10) >> 128
    if (abs_tick & 2048) != 0:
        ratio = unsafe_mul(ratio, M11) >> 128
    if (abs_tick & 4096) != 0:
        ratio = unsafe_mul(ratio, M12) >> 128
    if (abs_tick & 8192) != 0:
        ratio = unsafe_mul(ratio, M13) >> 128
    if (abs_tick & 16384) != 0:
        ratio = unsafe_mul(ratio, M14) >> 128
    if (abs_tick & 32768) != 0:
        ratio = unsafe_mul(ratio, M15) >> 128
    if (abs_tick & 65536) != 0:
        ratio = unsafe_mul(ratio, M16) >> 128
    if (abs_tick & 131072) != 0:
        ratio = unsafe_mul(ratio, M17) >> 128
    if (abs_tick & 262144) != 0:
        ratio = unsafe_mul(ratio, M18) >> 128
    if (abs_tick & 524288) != 0:
        ratio = unsafe_mul(ratio, M19) >> 128

    if tick > 0:
        ratio = max_value(uint256) // ratio

    # Divide by 1<<32 rounding up to go from Q128.128 to Q128.96,
    # then downcast to uint160 (always fits for valid ticks).
    # Round up so getTickAtSqrtRatio of the output price is consistent.
    return convert((ratio >> 32) + convert(ratio % (1 << 32) > 0, uint256), uint160)


@internal
@pure
def get_tick_at_sqrt_ratio(sqrt_price_x96: uint160) -> int24:
    """Calculates the greatest tick value such that getRatioAtTick(tick) <= ratio.

    Throws if sqrtPriceX96 < MIN_SQRT_RATIO or >= MAX_SQRT_RATIO.
    """
    assert sqrt_price_x96 >= MIN_SQRT_RATIO and sqrt_price_x96 < MAX_SQRT_RATIO, "R"

    ratio: uint256 = convert(sqrt_price_x96, uint256) << 32

    # ── Compute MSB (most significant bit position) ──
    # In Solidity, this is done with a series of assembly blocks doing
    # binary search on bit ranges. We replicate with pure Vyper comparisons.
    r: uint256 = ratio
    msb: uint256 = 0

    if r > 340282366920938463463374607431768211455:  # > 2^128-1
        msb = msb + 128
        r = r >> 128
    if r > 18446744073709551615:  # > 2^64-1
        msb = msb + 64
        r = r >> 64
    if r > 4294967295:  # > 2^32-1
        msb = msb + 32
        r = r >> 32
    if r > 65535:  # > 2^16-1
        msb = msb + 16
        r = r >> 16
    if r > 255:  # > 2^8-1
        msb = msb + 8
        r = r >> 8
    if r > 15:  # > 2^4-1
        msb = msb + 4
        r = r >> 4
    if r > 3:  # > 2^2-1
        msb = msb + 2
        r = r >> 2
    if r > 1:  # > 2^1-1
        msb = msb + 1

    # ── Compute log2 via iterative squaring ──
    if msb >= 128:
        r = ratio >> unsafe_sub(msb, 127)
    else:
        r = ratio << unsafe_sub(127, msb)

    log_2: int256 = (convert(msb, int256) - 128) << 64

    # Each iteration: r = r*r >> 127; f = r >> 128; log_2 |= f << (63-i)
    r = unsafe_mul(r, r) >> 127
    f: uint256 = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 63)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 62)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 61)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 60)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 59)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 58)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 57)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 56)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 55)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 54)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 53)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 52)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 51)
    r = r >> f

    r = unsafe_mul(r, r) >> 127
    f = r >> 128
    log_2 = log_2 | unsafe_mul(convert(f, int256), 1 << 50)

    # ── Convert log2 to tick ──
    log_sqrt10001: int256 = unsafe_mul(log_2, 255738958999603826347141)  # 128.128 fixed point

    tick_low: int24 = convert(
        unsafe_sub(log_sqrt10001, 3402992956809132418596140100660247210) >> 128,
        int24
    )
    tick_hi: int24 = convert(
        unsafe_add(log_sqrt10001, 291339464771989622907027621153398088495) >> 128,
        int24
    )

    if tick_low == tick_hi:
        return tick_low
    elif self.get_sqrt_ratio_at_tick(tick_hi) <= sqrt_price_x96:
        return tick_hi
    else:
        return tick_low
