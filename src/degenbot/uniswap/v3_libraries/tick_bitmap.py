from decimal import Decimal
from functools import lru_cache

from degenbot.constants import MAX_UINT8
from degenbot.exceptions import DegenbotValueError, LiquidityMapWordMissing
from degenbot.logging import logger
from degenbot.uniswap.types import UniswapV3BitmapAtWord
from degenbot.uniswap.v3_libraries.bit_math import least_significant_bit, most_significant_bit


def flip_tick(
    tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    update_block: int | None = None,
) -> None:
    if not (tick % tick_spacing == 0):
        raise DegenbotValueError(message="Tick not correctly spaced!")

    word_pos, bit_pos = position(int(Decimal(tick) // tick_spacing))
    logger.debug(f"Flipping {tick=} @ {word_pos=}, {bit_pos=}")

    if word_pos not in tick_bitmap:
        raise LiquidityMapWordMissing(word_pos)

    mask = 1 << bit_pos
    tick_bitmap[word_pos] = UniswapV3BitmapAtWord(
        bitmap=tick_bitmap[word_pos].bitmap ^ mask,
        block=update_block,
    )
    logger.debug(f"Flipped {tick=} @ {word_pos=}, {bit_pos=}")


@lru_cache
def position(tick: int) -> tuple[int, int]:
    word_pos = tick >> 8
    bit_pos = tick % 256
    return word_pos, bit_pos


def next_initialized_tick_within_one_word(
    tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> tuple[int, bool]:
    compressed = int(
        # Uses Decimal so floor division of negative ticks round to zero, matching EVM
        Decimal(tick) // tick_spacing
    )
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
