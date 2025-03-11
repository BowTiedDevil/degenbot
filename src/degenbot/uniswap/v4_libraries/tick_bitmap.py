import bisect
from collections.abc import Generator
from itertools import count

from degenbot.exceptions import LiquidityMapWordMissing
from degenbot.uniswap.types import UniswapV4BitmapAtWord, UniswapV4LiquidityAtTick


# @dev round towards negative infinity
def compress(tick: int, tick_spacing: int) -> int:
    """
    Compress the given tick by the spacing, rounding down towards negative infinity
    """
    # Use Python floor division directly, which matches rounding specified by the function
    return tick // tick_spacing


# @notice Computes the position in the mapping where the initialized bit for a tick lives
# @param tick The tick for which to compute the position
# @return wordPos The key in the mapping containing the word in which the bit is stored
# @return bitPos The bit position in the word where the flag is stored
def position(tick: int) -> tuple[int, int]:
    return tick >> 8, tick % 256


def gen_ticks(
    tick_data: dict[int, UniswapV4LiquidityAtTick],
    starting_tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> Generator[tuple[int, bool], None, None]:
    """
    Yields ticks from the set of all possible ticks at 32 byte (256 bit) word boundaries and
    initialized ticks found in the liquidity mapping. The ticks are yielded in descending order when
    `less_than_or_equal` is True, else ascending.
    """

    # Python rounds down to negative infinity, so use it directly instead of the abs and modulo
    # implementation of the Solidity contract
    compressed = starting_tick // tick_spacing
    word_pos, _ = position(compressed)

    # The boundary ticks for each word are at the 0th and 255th bits.
    # On the way down (less_than_or_equal=True), start at the 0th bit.
    # On the way up (less_than_or_equal=False), start at the 255th bit.
    if less_than_or_equal:
        step_distance = -256 * tick_spacing
        first_boundary_tick = tick_spacing * 256 * word_pos
    else:
        step_distance = 256 * tick_spacing
        first_boundary_tick = tick_spacing * (256 * word_pos + 255)
        if starting_tick >= first_boundary_tick:
            # Special case: starting tick on the first word boundary, begin at the next word
            first_boundary_tick += 256 * tick_spacing
    boundary_ticks_iter = count(
        start=first_boundary_tick,
        step=step_distance,
    )

    # All initialized ticks are known from the tick_data mapping.
    initialized_ticks_iter = iter(
        sorted((tick for tick in tick_data if tick <= starting_tick), reverse=True)
        if less_than_or_equal
        else sorted(tick for tick in tick_data if tick > starting_tick)
    )

    next_initialized_tick = next(initialized_ticks_iter, None)
    next_boundary_tick = next(boundary_ticks_iter)

    if less_than_or_equal:
        # Yield the greater of the nearest initialized and boundary tick until the initialized ticks
        # are exhausted
        while next_initialized_tick is not None:
            if next_initialized_tick > next_boundary_tick:
                yield (next_initialized_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
            elif next_boundary_tick > next_initialized_tick:
                yield (next_boundary_tick, False)
                next_boundary_tick = next(boundary_ticks_iter)
            else:
                # The next initialized tick lies on a boundary, so advance both iterators
                yield (next_boundary_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
                next_boundary_tick = next(boundary_ticks_iter)
    else:
        # Yield the lesser of the nearest initialized and boundary tick until the initialized ticks
        # are exhausted
        while next_initialized_tick is not None:
            if next_initialized_tick < next_boundary_tick:
                yield (next_initialized_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
            elif next_boundary_tick < next_initialized_tick:
                yield (next_boundary_tick, False)
                next_boundary_tick = next(boundary_ticks_iter)
            else:
                # The next initialized tick lies on a boundary, so advance both iterators
                yield (next_boundary_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
                next_boundary_tick = next(boundary_ticks_iter)

    # Then yield uninitialized boundary ticks forever
    while True:
        yield (next_boundary_tick, False)
        next_boundary_tick = next(boundary_ticks_iter)


def next_initialized_tick_within_one_word(
    tick_bitmap: dict[int, UniswapV4BitmapAtWord],
    tick_data: dict[int, UniswapV4LiquidityAtTick],
    tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> tuple[int, bool]:
    compressed = -(-tick // tick_spacing) if tick < 0 else tick // tick_spacing
    if tick < 0 and tick % tick_spacing != 0:
        compressed -= 1  # round towards negative infinity

    if less_than_or_equal:
        if tick in tick_data:
            return tick, True

        word_pos, _ = position(compressed)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        lowest_tick_in_word = tick_spacing * (256 * word_pos)
        known_ticks = sorted(tick_data)
        tick_index = bisect.bisect_left(known_ticks, tick)

        next_tick = (
            lowest_tick_in_word
            if tick_index == 0
            else max(lowest_tick_in_word, known_ticks[tick_index - 1])
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, _ = position(compressed + 1)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        highest_tick_in_word = tick_spacing * (256 * word_pos) + tick_spacing * 255
        known_ticks = sorted(tick_data)
        tick_index = bisect.bisect_right(known_ticks, tick)

        next_tick = (
            highest_tick_in_word
            if tick_index == len(known_ticks)
            else min(highest_tick_in_word, known_ticks[tick_index])
        )

    return next_tick, next_tick in tick_data
