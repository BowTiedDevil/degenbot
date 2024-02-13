from typing import Dict

import pytest
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_dataclasses import UniswapV3BitmapAtWord
from degenbot.uniswap.v3_libraries import TickBitmap, TickMath
from degenbot.uniswap.v3_libraries.tick_bitmap import MissingTickWordError

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickBitmap.spec.ts


def is_initialized(tick_bitmap: Dict[int, UniswapV3BitmapAtWord], tick: int) -> bool:
    # Adapted from Uniswap test contract
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/test/TickBitmapTest.sol

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(tick_bitmap, tick, 1, True)
    return next == tick if initialized else False


def empty_full_bitmap(spacing: int = 1):
    """
    Generate a empty tick bitmap, maximum size, with the given tick spacing
    """

    tick_bitmap = {}
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK, spacing):
        wordPos, _ = TickBitmap.position(tick=tick)
        tick_bitmap[wordPos] = UniswapV3BitmapAtWord()
    return tick_bitmap


def empty_sparse_bitmap():
    """
    Generate a sparse, empty tick bitmap no populated words
    """

    tick_bitmap = {}
    return tick_bitmap


def test_isInitialized():
    tick_bitmap = empty_full_bitmap()

    assert is_initialized(tick_bitmap, 1) is False

    TickBitmap.flipTick(tick_bitmap, 1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is True

    # TODO: The repo flips this tick twice, which may be a mistake
    # TickBitmap.flipTick(tick_bitmap, 1, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is False

    TickBitmap.flipTick(tick_bitmap, tick=2, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is False

    TickBitmap.flipTick(tick_bitmap, tick=1 + 256, tick_spacing=1)
    assert is_initialized(tick_bitmap, 257) is True
    assert is_initialized(tick_bitmap, 1) is False


def test_flipTick() -> None:
    tick_bitmap = empty_full_bitmap()

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) is True
    assert is_initialized(tick_bitmap, -231) is False
    assert is_initialized(tick_bitmap, -229) is False
    assert is_initialized(tick_bitmap, -230 + 256) is False
    assert is_initialized(tick_bitmap, -230 - 256) is False

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) is False
    assert is_initialized(tick_bitmap, -231) is False
    assert is_initialized(tick_bitmap, -229) is False
    assert is_initialized(tick_bitmap, -230 + 256) is False
    assert is_initialized(tick_bitmap, -230 - 256) is False

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-229, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=500, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-229, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)

    assert is_initialized(tick_bitmap, -259) is True
    assert is_initialized(tick_bitmap, -229) is False


def test_flipTick_sparse() -> None:
    tick_bitmap = empty_sparse_bitmap()
    with pytest.raises(MissingTickWordError, match="Called flipTick on missing word"):
        TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)


def test_incorrect_tick_spacing_flip() -> None:
    tick_spacing = 3
    tick_bitmap = empty_full_bitmap(tick_spacing)
    with pytest.raises(EVMRevertError, match="Tick not correctly spaced"):
        TickBitmap.flipTick(tick_bitmap, tick=2, tick_spacing=tick_spacing)


def test_nextInitializedTickWithinOneWord() -> None:
    tick_bitmap: Dict[int, UniswapV3BitmapAtWord] = {}

    # set up a full-sized empty tick bitmap
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK):
        wordPos, _ = TickBitmap.position(tick=tick)
        if not tick_bitmap.get(wordPos):
            tick_bitmap[wordPos] = UniswapV3BitmapAtWord()

    # set the specified ticks to initialized
    for tick in [-200, -55, -4, 70, 78, 84, 139, 240, 535]:
        TickBitmap.flipTick(tick_bitmap=tick_bitmap, tick=tick, tick_spacing=1)

    # lte = false tests

    # returns tick to right if at initialized tick
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 84
    assert initialized is True

    # returns tick to right if at initialized tick
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-55,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -4
    assert initialized is True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=77,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 78
    assert initialized is True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-56,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -55
    assert initialized is True

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized is False

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -200
    assert initialized is True

    # does not exceed boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=508,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized is False

    # skips entire word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized is False

    # skips half word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=383,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized is False

    # lte = true tests

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 78
    assert initialized is True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=79,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 78
    assert initialized is True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=258,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 256
    assert initialized is False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=256,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 256
    assert initialized is False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=72,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 70
    assert initialized is True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == -512
    assert initialized is False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=1023,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 768
    assert initialized is False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 768
    assert initialized is False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=1,
        less_than_or_equal=True,
    )
