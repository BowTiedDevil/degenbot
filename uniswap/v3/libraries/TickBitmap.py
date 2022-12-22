from typing import Tuple

from degenbot.exceptions import DegenbotError

from . import BitMath
from .Helpers import *

MAXUINT8 = 2**8 - 1


class BitmapWordUnavailable(DegenbotError):
    pass


def position(tick: int) -> Tuple[int, int]:
    wordPos: int = int16(tick >> 8)
    bitPos: int = uint8(tick % 256)
    return (wordPos, bitPos)


def nextInitializedTickWithinOneWord(
    tickBitmap: int,
    tick: int,
    tickSpacing: int,
    lte: bool,
) -> Tuple[int, bool]:

    compressed: int = tick // tickSpacing
    if tick < 0 and tick % tickSpacing != 0:
        compressed -= 1  # round towards negative infinity

    if lte:
        wordPos, bitPos = position(compressed)
        # all the 1s at or to the right of the current bitPos
        mask: int = (1 << bitPos) - 1 + (1 << bitPos)
        if (bitmap_word := tickBitmap.get(wordPos)) is not None:
            masked: int = bitmap_word & mask
        else:
            raise BitmapWordUnavailable(wordPos)

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

        if (bitmap_word := tickBitmap.get(wordPos)) is not None:
            masked: int = bitmap_word & mask
        else:
            raise BitmapWordUnavailable(wordPos)

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
            else (compressed + 1 + int24(MAXUINT8 - bitPos)) * tickSpacing
        )

    return next_tick, initialized_status
