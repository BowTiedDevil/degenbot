from typing import Any

import pytest

from degenbot.exceptions import EVMRevertError, MissingTickWordError
from degenbot.uniswap.v3_libraries import TickBitmap, TickMath
from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickBitmap.spec.ts


def is_initialized(tick_bitmap: dict[int, UniswapV3BitmapAtWord], tick: int) -> bool:
    # Adapted from Uniswap test contract
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/test/TickBitmapTest.sol

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(tick_bitmap, tick, 1, True)
    return next == tick if initialized else False


def empty_full_bitmap(spacing: int = 1) -> dict[int, UniswapV3BitmapAtWord]:
    """
    Generate a empty tick bitmap, maximum size, with the given tick spacing
    """

    tick_bitmap = {}
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK, spacing):
        wordPos, _ = TickBitmap.position(tick=tick)
        tick_bitmap[wordPos] = UniswapV3BitmapAtWord()
    return tick_bitmap


def empty_sparse_bitmap() -> dict[int, Any]:
    """
    Generate a sparse, empty tick bitmap
    """
    return dict()


def test_isInitialized():
    tick_bitmap = empty_full_bitmap()

    assert is_initialized(tick_bitmap, 1) is False

    TickBitmap.flipTick(tick_bitmap, 1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is True

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
    TICK_SPACING = 1
    INITIALIZED_TICKS = [-200, -55, -4, 70, 78, 84, 139, 240, 535]

    # set up a full-sized empty tick bitmap, then initialize the ticks required for the tests
    tick_bitmap: dict[int, UniswapV3BitmapAtWord] = {}
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK, TICK_SPACING):
        wordPos, _ = TickBitmap.position(tick=tick)
        if not tick_bitmap.get(wordPos):
            tick_bitmap[wordPos] = UniswapV3BitmapAtWord()
    for tick in INITIALIZED_TICKS:
        TickBitmap.flipTick(tick_bitmap=tick_bitmap, tick=tick, tick_spacing=1)

    # lte = false tests

    # returns tick to right if at initialized tick
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (84, True)

    # returns tick to right if at initialized tick
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-55,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (-4, True)

    # returns the tick directly to the right
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=77,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (78, True)

    # returns the tick directly to the right
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-56,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (-55, True)

    # returns the next words initialized tick if on the right boundary
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (511, False)

    # returns the next words initialized tick if on the right boundary
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (-200, True)

    # does not exceed boundary
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=508,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (511, False)

    # skips entire word
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (511, False)

    # skips half word
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=383,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=False,
    ) == (511, False)

    # lte = true tests

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (78, True)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=79,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (78, True)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=258,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (256, False)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=256,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (256, False)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=72,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (70, True)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (-512, False)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=1023,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (768, False)

    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (768, False)

    TickBitmap.flipTick(tick_bitmap=tick_bitmap, tick=329, tick_spacing=1)
    assert TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=456,
        tick_spacing=TICK_SPACING,
        less_than_or_equal=True,
    ) == (329, True)
