import bisect
from collections.abc import Generator
from decimal import Decimal
from functools import lru_cache
from itertools import count

from degenbot.constants import MAX_UINT8
from degenbot.exceptions import DegenbotValueError, LiquidityMapWordMissing
from degenbot.uniswap.types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick
from degenbot.uniswap.v3_libraries._config import LRU_CACHE_SIZE
from degenbot.uniswap.v3_libraries.bit_math import least_significant_bit, most_significant_bit


def flip_tick(
    tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    update_block: int | None = None,
) -> None:
    if tick % tick_spacing != 0:
        raise DegenbotValueError(message="Tick not correctly spaced!")

    word_pos, bit_pos = position(-(-tick // tick_spacing) if tick < 0 else tick // tick_spacing)

    if word_pos not in tick_bitmap:
        raise LiquidityMapWordMissing(word_pos)

    tick_bitmap[word_pos] = UniswapV3BitmapAtWord(
        bitmap=tick_bitmap[word_pos].bitmap ^ (1 << bit_pos),
        block=update_block,
    )


@lru_cache(maxsize=LRU_CACHE_SIZE)
def position(tick: int) -> tuple[int, int]:
    return (
        tick >> 8,  # word_pos
        tick % 256,  # bit_pos
    )


def next_initialized_tick_within_one_word_legacy(
    tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> tuple[int, bool]:
    compressed = -(-tick // tick_spacing) if tick < 0 else tick // tick_spacing
    if tick < 0 and tick % tick_spacing != 0:
        compressed -= 1  # round towards negative infinity

    if less_than_or_equal:
        word_pos, bit_pos = position(compressed)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        bitmap_at_word = tick_bitmap[word_pos].bitmap
        mask = 2 * (1 << bit_pos) - 1  # all the 1s at or to the right of the current bitPos
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the right of or at the current tick, return rightmost
        # in the word
        initialized_status = masked != 0
        next_tick = (
            (compressed - (bit_pos - most_significant_bit(masked))) * tick_spacing
            if initialized_status
            else (compressed - bit_pos) * tick_spacing
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, bit_pos = position(compressed + 1)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        bitmap_at_word = tick_bitmap[word_pos].bitmap
        mask = ~((1 << bit_pos) - 1)  # all the 1s at or to the left of the bitPos
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the left of the current tick, return leftmost in the
        # word
        initialized_status = masked != 0
        next_tick = (
            (compressed + 1 + (least_significant_bit(masked) - bit_pos)) * tick_spacing
            if initialized_status
            else (compressed + 1 + (MAX_UINT8 - bit_pos)) * tick_spacing
        )

    return next_tick, initialized_status


def gen_ticks(
    tick_data: dict[int, UniswapV3LiquidityAtTick],
    starting_tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> Generator[tuple[int, bool], None, None]:
    compressed = (
        -(-starting_tick // tick_spacing) if starting_tick < 0 else starting_tick // tick_spacing
    )
    if starting_tick < 0 and starting_tick % tick_spacing != 0:
        compressed -= 1  # round towards negative infinity

    word_pos, _ = position(compressed)

    if less_than_or_equal:
        lowest_tick_in_starting_word = tick_spacing * (256 * word_pos)
        initialized_ticks = sorted(
            (tick for tick in tick_data if tick <= starting_tick), reverse=True
        )

        boundary_ticks_iter = count(lowest_tick_in_starting_word, -256 * tick_spacing)
        initialized_tick_iter = iter(initialized_ticks)

        next_initialized_tick = next(initialized_tick_iter, None)
        next_boundary_tick = next(boundary_ticks_iter, None)

        while True:
            # Always return the highest value, since this generator will be descending
            match next_initialized_tick, next_boundary_tick:
                case int(), int():
                    if next_initialized_tick > next_boundary_tick:
                        yield (next_initialized_tick, True)
                        next_initialized_tick = next(initialized_tick_iter, None)
                    elif next_boundary_tick > next_initialized_tick:
                        yield (next_boundary_tick, False)
                        next_boundary_tick = next(boundary_ticks_iter)
                    else:
                        # Initialized tick on a boundary
                        yield (next_initialized_tick, True)
                        next_initialized_tick = next(initialized_tick_iter, None)
                        next_boundary_tick = next(boundary_ticks_iter)
                case None, int():
                    yield (next_boundary_tick, False)
                    next_boundary_tick = next(boundary_ticks_iter)

    else:
        highest_tick_in_starting_word = tick_spacing * (256 * word_pos + 255)
        initialized_ticks = sorted(tick for tick in tick_data if tick > starting_tick)

        initialized_tick_iter = iter(initialized_ticks)
        boundary_ticks_iter = count(highest_tick_in_starting_word, 256 * tick_spacing)

        next_initialized_tick = next(initialized_tick_iter, None)
        next_boundary_tick = next(boundary_ticks_iter)

        while True:
            match next_initialized_tick, next_boundary_tick:
                case int(), int():
                    if next_initialized_tick < next_boundary_tick:
                        yield (next_initialized_tick, True)
                        next_initialized_tick = next(initialized_tick_iter, None)
                    elif next_boundary_tick < next_initialized_tick:
                        yield (next_boundary_tick, False)
                        next_boundary_tick = next(boundary_ticks_iter)
                    else:
                        # Initialized tick on a boundary
                        yield (next_initialized_tick, True)
                        next_initialized_tick = next(initialized_tick_iter, None)
                        next_boundary_tick = next(boundary_ticks_iter)
                case None, int():
                    yield (next_boundary_tick, False)
                    next_boundary_tick = next(boundary_ticks_iter)


def next_initialized_tick_within_one_word(
    tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    tick_data: dict[int, UniswapV3LiquidityAtTick],
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
