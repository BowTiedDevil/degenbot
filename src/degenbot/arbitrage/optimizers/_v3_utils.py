"""V3/V4 tick and virtual-reserve helpers used by piecewise and Brent solvers."""

from degenbot.types.hop_types import V3TickRangeInfo
from degenbot.uniswap.v3_libraries.constants import Q96
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK, get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


def _v3_virtual_reserves(
    *,
    liquidity: int,
    sqrt_price_x96: int,
    zero_for_one: bool,
) -> tuple[int, int]:
    """
    Compute virtual reserves for a V3/V4 tick range.

    For a concentrated-liquidity pool, the effective (virtual) reserves
    within the current tick range are:
        R0_virtual = L / sqrt_price
        R1_virtual = L * sqrt_price

    where sqrt_price = sqrt_price_x96 / 2**96.

    The reserves are returned as integers scaled to match V2 wei-scale
    reserve magnitudes for compatibility with the Möbius solver.

    Parameters
    ----------
    liquidity
        V3/V4 liquidity in this tick range.
    sqrt_price_x96
        Current sqrt price as Q64.96 fixed-point.
    zero_for_one
        True if swapping token0 → token1 (input is token0).

    Returns
    -------
    tuple[int, int]
        (reserve_in, reserve_out) as integers in wei-equivalent scale.
    """
    # Convert X96 to float for virtual reserve computation
    sqrt_price = sqrt_price_x96 / Q96
    liq = float(liquidity)

    r0_virtual = liq / sqrt_price
    r1_virtual = liq * sqrt_price

    # Scale to integer — multiply by Q96 to preserve precision
    # The Möbius solver uses float internally, so the exact integer
    # scale doesn't matter as long as reserves are in the right ratio.
    # Using int(round()) to avoid drift.
    scale = Q96
    if zero_for_one:
        return round(r0_virtual * scale), round(r1_virtual * scale)
    return round(r1_virtual * scale), round(r0_virtual * scale)


# Cache for tick range lookups: (pool_address, current_tick, zero_for_one) -> result
_tick_range_cache: dict[tuple[str, int, bool], tuple[tuple[V3TickRangeInfo, ...], int] | None] = {}
_MAX_TICK_RANGE_CACHE_SIZE = 128


def _get_cached_tick_ranges(
    *,
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Cached version of _v3_get_adjacent_tick_ranges.

    Uses LRU-style cache keyed by (pool_address, current_tick, zero_for_one).
    Cache is cleared when it exceeds _MAX_TICK_RANGE_CACHE_SIZE entries.
    """
    cache_key = (str(pool.address), pool.tick, zero_for_one)

    # Check cache
    if cache_key in _tick_range_cache:
        return _tick_range_cache[cache_key]

    # Compute and cache result
    result = _v3_get_adjacent_tick_ranges(
        pool=pool,
        zero_for_one=zero_for_one,
        max_ranges=max_ranges,
    )

    # Simple LRU: clear if too large (simplest approach)
    if len(_tick_range_cache) >= _MAX_TICK_RANGE_CACHE_SIZE:
        _tick_range_cache.clear()

    _tick_range_cache[cache_key] = result
    return result


def _v3_get_adjacent_tick_ranges(
    *,
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Fetch adjacent tick ranges from a V3/V4 pool for multi-range support.

    Returns a tuple of (tick_ranges, current_range_index) where current_range_index
    indicates which range contains the current price. Returns None if the pool
    doesn't have full tick data available (sparse liquidity map).

    Parameters
    ----------
    pool
        A UniswapV3Pool or UniswapV4Pool.
    zero_for_one
        True if swapping token0 → token1.
    max_ranges
        Maximum number of ranges to fetch (including current).

    Returns
    -------
    tuple[tuple[V3TickRangeInfo, ...], int] | None
        Adjacent tick ranges and current range index, or None if sparse.
    """

    # Check if pool has full tick data (sparse pools can't provide adjacent ranges)
    if getattr(pool, "sparse_liquidity_map", True):
        return None

    tick_data = getattr(pool, "tick_data", None)
    tick_bitmap = getattr(pool, "tick_bitmap", None)
    tick_spacing = getattr(pool, "tick_spacing", 0)

    if tick_data is None or tick_bitmap is None or tick_spacing == 0:
        return None

    current_tick = pool.tick

    # Generate ticks in swap direction
    less_than_or_equal = not zero_for_one  # token0→token1: price goes down, tick goes down

    ticks_along_path = gen_ticks(
        tick_data=tick_data,
        starting_tick=current_tick,
        tick_spacing=tick_spacing,
        less_than_or_equal=less_than_or_equal,
    )

    # Build list of initialized ticks
    # Clamp ticks to MIN_TICK/MAX_TICK bounds like real V3 pool does
    initialized_ticks: list[int] = []
    try:
        for tick, is_initialized in ticks_along_path:
            # Clamp to valid tick range (like UniswapV3Pool._calculate_swap)
            clamped_tick = (
                max(MIN_TICK, tick)  # descending ticks
                if less_than_or_equal
                else min(MAX_TICK, tick)  # ascending ticks
            )

            # Stop if we've reached the boundary
            if clamped_tick != tick:
                break

            if len(initialized_ticks) >= max_ranges + 1:
                break
            if is_initialized or tick == current_tick:
                initialized_ticks.append(tick)
    except StopIteration:
        pass

    if len(initialized_ticks) < 2:
        # Not enough range boundaries to form meaningful ranges
        return None

    # Build V3TickRangeInfo for each range
    ranges: list[V3TickRangeInfo] = []
    current_idx = 0

    for i in range(len(initialized_ticks) - 1):
        if zero_for_one:
            tick_lower = initialized_ticks[i + 1]
            tick_upper = initialized_ticks[i]
        else:
            tick_lower = initialized_ticks[i]
            tick_upper = initialized_ticks[i + 1]

        # Get liquidity at this tick
        tick_info = tick_data.get(tick_lower if zero_for_one else tick_upper)
        liquidity = tick_info.liquidity_net if tick_info else pool.liquidity

        # Compute sqrt price bounds
        sqrt_price_lower = int(get_sqrt_ratio_at_tick(tick_lower))
        sqrt_price_upper = int(get_sqrt_ratio_at_tick(tick_upper))

        range_info = V3TickRangeInfo(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity=liquidity,
            sqrt_price_lower=sqrt_price_lower,
            sqrt_price_upper=sqrt_price_upper,
        )
        ranges.append(range_info)

        # Determine if this range contains current price
        if zero_for_one:
            if tick_lower <= current_tick < tick_upper:
                current_idx = i
        elif tick_lower <= current_tick < tick_upper:
            current_idx = i

    if len(ranges) < 1:
        return None

    return (tuple(ranges), current_idx)
