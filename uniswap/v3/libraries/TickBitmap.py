from decimal import Decimal
from typing import Dict, Optional, Tuple

from degenbot.constants import MAX_UINT8
from degenbot.exceptions import (
    BitmapWordUnavailableError,
    EVMRevertError,
    MissingTickWordError,
)
from degenbot.logging import logger
from degenbot.uniswap.v3.libraries import BitMath
from degenbot.uniswap.v3.libraries.functions import int16, int24, uint8
from degenbot.uniswap.v3.v3_liquidity_pool import UniswapV3BitmapAtWord


def flipTick(
    tick_bitmap: Dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    update_block: Optional[int] = None,
):
    if not (tick % tick_spacing == 0):
        raise EVMRevertError("Tick not correctly spaced!")

    word_pos, bit_pos = position(int(Decimal(tick) // tick_spacing))
    logger.debug(f"Flipping {tick=} @ {word_pos=}, {bit_pos=}")

    try:
        mask = 1 << bit_pos
        tick_bitmap[word_pos].bitmap ^= mask
        tick_bitmap[word_pos].block = update_block
    except KeyError:
        raise MissingTickWordError(
            f"Called flipTick on missing word={word_pos}"
        )
    else:
        logger.debug(f"Flipped {tick=} @ {word_pos=}, {bit_pos=}")


def position(tick: int) -> Tuple[int, int]:
    word_pos: int = int16(tick >> 8)
    bit_pos: int = uint8(tick % 256)
    return (word_pos, bit_pos)


def nextInitializedTickWithinOneWord(
    tick_bitmap: Dict[int, UniswapV3BitmapAtWord],
    tick: int,
    tick_spacing: int,
    less_than_or_equal: bool,
) -> Tuple[int, bool]:
    compressed = int(
        Decimal(tick) // tick_spacing
    )  # tick can be negative, use Decimal so floor division rounds to zero instead of negative infinity
    if tick < 0 and tick % tick_spacing != 0:
        compressed -= 1  # round towards negative infinity

    if less_than_or_equal:
        word_pos, bit_pos = position(compressed)

        try:
            bitmap_at_word = tick_bitmap[word_pos].bitmap
        except KeyError:
            raise BitmapWordUnavailableError(word_pos)

        # all the 1s at or to the right of the current bitPos
        mask = 2 * (1 << bit_pos) - 1
        masked = bitmap_at_word & mask

        # if there are no initialized ticks to the right of or at the current tick, return rightmost in the word
        initialized_status = masked != 0
        # overflow/underflow is possible, but prevented externally by limiting both tickSpacing and tick
        next_tick = (
            (compressed - int24(bit_pos - BitMath.mostSignificantBit(masked)))
            * tick_spacing
            if initialized_status
            else (compressed - int24(bit_pos)) * tick_spacing
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, bit_pos = position(compressed + 1)

        try:
            bitmap_at_word = tick_bitmap[word_pos].bitmap
        except KeyError:
            raise BitmapWordUnavailableError(word_pos)

        # all the 1s at or to the left of the bitPos
        mask = ~((1 << bit_pos) - 1)
        masked = bitmap_at_word & mask

        # if there are no initialized ticks to the left of the current tick, return leftmost in the word
        initialized_status = masked != 0
        # overflow/underflow is possible, but prevented externally by limiting both tickSpacing and tick
        next_tick = (
            (
                compressed
                + 1
                + int24(BitMath.leastSignificantBit(masked) - bit_pos)
            )
            * tick_spacing
            if initialized_status
            else (compressed + 1 + int24(MAX_UINT8 - bit_pos)) * tick_spacing
        )

    return next_tick, initialized_status
