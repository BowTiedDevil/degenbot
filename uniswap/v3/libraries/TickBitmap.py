from typing import Tuple, Optional

from degenbot.exceptions import (
    MissingTickWordError,
    BitmapWordUnavailableError,
)

from . import BitMath
from .Helpers import *

from decimal import Decimal


def flipTick(
    tickBitmap: dict,
    tick: int,
    tickSpacing: int,
    update_block: Optional[int] = None,
):

    if not (tick % tickSpacing == 0):
        raise EVMRevertError("Tick not correctly spaced!")

    wordPos, bitPos = position(int(Decimal(tick) // tickSpacing))
    # print(f"flipping {tick=} @ {wordPos=}, {bitPos=}")

    # if tickBitmap.get(wordPos) is None:
    #     raise MissingTickWordError(
    #         f"Called flipTick on missing word: {wordPos=}"
    #     )

    try:
        mask = 1 << bitPos
        tickBitmap[wordPos]["bitmap"] ^= mask
        tickBitmap[wordPos]["block"] = update_block
    except KeyError:
        raise MissingTickWordError(
            f"Called flipTick on missing word: {wordPos=}"
        )


def position(tick: int) -> Tuple[int, int]:
    wordPos: int = int16(tick >> 8)
    bitPos: int = uint8(tick % 256)
    return (wordPos, bitPos)


def nextInitializedTickWithinOneWord(
    tickBitmap: dict,
    tick: int,
    tickSpacing: int,
    lte: bool,
) -> Tuple[int, bool]:

    compressed: int = int(
        Decimal(tick) // tickSpacing
    )  # tick can be negative, use Decimal so floor division rounds to zero instead of negative infinity
    if tick < 0 and tick % tickSpacing != 0:
        compressed -= 1  # round towards negative infinity

    if lte:
        wordPos, bitPos = position(compressed)
        # all the 1s at or to the right of the current bitPos
        mask: int = (1 << bitPos) - 1 + (1 << bitPos)

        try:
            bitmap_word = tickBitmap[wordPos]["bitmap"]
        except:
            raise BitmapWordUnavailableError(wordPos)
        else:
            masked: int = bitmap_word & mask

        # if there are no initialized ticks to the right of or at the current tick, return rightmost in the word
        initialized_status: bool = masked != 0
        # overflow/underflow is possible, but prevented externally by limiting both tickSpacing and tick
        next_tick = (
            (compressed - int24(bitPos - BitMath.mostSignificantBit(masked)))
            * tickSpacing
            if initialized_status
            else (compressed - int24(bitPos)) * tickSpacing
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        wordPos, bitPos = position(compressed + 1)
        # all the 1s at or to the left of the bitPos
        mask: int = ~((1 << bitPos) - 1)

        try:
            bitmap_word = tickBitmap[wordPos]["bitmap"]
        except:
            raise BitmapWordUnavailableError(wordPos)
        else:
            masked: int = bitmap_word & mask

        # if there are no initialized ticks to the left of the current tick, return leftmost in the word
        initialized_status: bool = masked != 0
        # overflow/underflow is possible, but prevented externally by limiting both tickSpacing and tick
        next_tick = (
            (
                compressed
                + 1
                + int24(BitMath.leastSignificantBit(masked) - bitPos)
            )
            * tickSpacing
            if initialized_status
            else (compressed + 1 + int24(MAX_UINT8 - bitPos)) * tickSpacing
        )

    return next_tick, initialized_status
