from decimal import Decimal

from ...constants import MAX_UINT8
from ...exceptions import BitmapWordUnavailableError, EVMRevertError, MissingTickWordError
from ...logging import logger
from ..v3_dataclasses import UniswapV3BitmapAtWord
from . import bit_math as BitMath


def flipTick(
    tick_bitmap: dict[int, "UniswapV3BitmapAtWord"],
    tick: int,
    tick_spacing: int,
    update_block: int | None = None,
) -> None:
    if not (tick % tick_spacing == 0):
        raise EVMRevertError("Tick not correctly spaced!")

    word_pos, bit_pos = position(int(Decimal(tick) // tick_spacing))
    logger.debug(f"Flipping {tick=} @ {word_pos=}, {bit_pos=}")

    try:
        mask = 1 << bit_pos
        tick_bitmap[word_pos].bitmap ^= mask
        tick_bitmap[word_pos].block = update_block
    except KeyError:
        raise MissingTickWordError(f"Called flipTick on missing word={word_pos}") from None
    else:
        logger.debug(f"Flipped {tick=} @ {word_pos=}, {bit_pos=}")


def position(tick: int) -> tuple[int, int]:
    word_pos: int = tick >> 8
    bit_pos: int = tick % 256
    return word_pos, bit_pos


def nextInitializedTickWithinOneWord(
    tick_bitmap: dict[int, "UniswapV3BitmapAtWord"],
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

        try:
            bitmap_at_word = tick_bitmap[word_pos].bitmap
        except KeyError:
            raise BitmapWordUnavailableError("Bitmap unavailable.", word_pos) from None

        # all the 1s at or to the right of the current bitPos
        mask = 2 * (1 << bit_pos) - 1
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the right of or at the current tick, return rightmost
        # in the word
        initialized_status = masked != 0
        next_tick = (
            (compressed - (bit_pos - BitMath.mostSignificantBit(masked))) * tick_spacing
            if initialized_status
            else (compressed - bit_pos) * tick_spacing
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, bit_pos = position(compressed + 1)

        try:
            bitmap_at_word = tick_bitmap[word_pos].bitmap
        except KeyError:
            raise BitmapWordUnavailableError("Bitmap unavailable.", word_pos) from None

        # all the 1s at or to the left of the bitPos
        mask = ~((1 << bit_pos) - 1)
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the left of the current tick, return leftmost in the
        # word
        initialized_status = masked != 0
        next_tick = (
            (compressed + 1 + (BitMath.leastSignificantBit(masked) - bit_pos)) * tick_spacing
            if initialized_status
            else (compressed + 1 + (MAX_UINT8 - bit_pos)) * tick_spacing
        )

    return next_tick, initialized_status
