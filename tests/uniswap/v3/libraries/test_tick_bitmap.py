from typing import Any

import pytest

from degenbot.exceptions import DegenbotValueError, LiquidityMapWordMissing
from degenbot.uniswap.types import UniswapV3BitmapAtWord
from degenbot.uniswap.v3_libraries.tick_bitmap import (
    flip_tick,
    next_initialized_tick_within_one_word,
    position,
)
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickBitmap.spec.ts


def is_initialized(tick_bitmap: dict[int, UniswapV3BitmapAtWord], tick: int) -> bool:
    # Adapted from Uniswap test contract
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/test/TickBitmapTest.sol

    next_tick, is_initialized = next_initialized_tick_within_one_word(tick_bitmap, tick, 1, True)
    return next_tick == tick if is_initialized else False


def empty_full_bitmap(spacing: int = 1) -> dict[int, UniswapV3BitmapAtWord]:
    """
    Generate a empty tick bitmap, maximum size, with the given tick spacing
    """

    tick_bitmap = {}
    for tick in range(MIN_TICK, MAX_TICK, spacing):
        word_pos, _ = position(tick=tick)
        tick_bitmap[word_pos] = UniswapV3BitmapAtWord()
    return tick_bitmap


def empty_sparse_bitmap() -> dict[int, Any]:
    """
    Generate a sparse, empty tick bitmap
    """
    return dict()


def test_is_initialized():
    tick_bitmap = empty_full_bitmap()

    assert is_initialized(tick_bitmap, 1) is False

    flip_tick(tick_bitmap, 1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is True

    flip_tick(tick_bitmap, tick=1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is False

    flip_tick(tick_bitmap, tick=2, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) is False

    flip_tick(tick_bitmap, tick=1 + 256, tick_spacing=1)
    assert is_initialized(tick_bitmap, 257) is True
    assert is_initialized(tick_bitmap, 1) is False


def test_flip_tick() -> None:
    tick_bitmap = empty_full_bitmap()

    flip_tick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) is True
    assert is_initialized(tick_bitmap, -231) is False
    assert is_initialized(tick_bitmap, -229) is False
    assert is_initialized(tick_bitmap, -230 + 256) is False
    assert is_initialized(tick_bitmap, -230 - 256) is False

    flip_tick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) is False
    assert is_initialized(tick_bitmap, -231) is False
    assert is_initialized(tick_bitmap, -229) is False
    assert is_initialized(tick_bitmap, -230 + 256) is False
    assert is_initialized(tick_bitmap, -230 - 256) is False

    flip_tick(tick_bitmap, tick=-230, tick_spacing=1)
    flip_tick(tick_bitmap, tick=-259, tick_spacing=1)
    flip_tick(tick_bitmap, tick=-229, tick_spacing=1)
    flip_tick(tick_bitmap, tick=500, tick_spacing=1)
    flip_tick(tick_bitmap, tick=-259, tick_spacing=1)
    flip_tick(tick_bitmap, tick=-229, tick_spacing=1)
    flip_tick(tick_bitmap, tick=-259, tick_spacing=1)

    assert is_initialized(tick_bitmap, -259) is True
    assert is_initialized(tick_bitmap, -229) is False


def test_flip_tick_sparse() -> None:
    tick_bitmap = empty_sparse_bitmap()
    with pytest.raises(LiquidityMapWordMissing):
        flip_tick(tick_bitmap, tick=-230, tick_spacing=1)


def test_incorrect_tick_spacing_flip() -> None:
    tick_spacing = 3
    tick_bitmap = empty_full_bitmap(tick_spacing)
    with pytest.raises(DegenbotValueError, match="Tick not correctly spaced"):
        flip_tick(tick_bitmap, tick=2, tick_spacing=tick_spacing)


def test_next_initialized_tick_within_one_word() -> None:
    tick_spacing = 1
    initialized_ticks = [-200, -55, -4, 70, 78, 84, 139, 240, 535]

    # set up a full-sized empty tick bitmap, then initialize the ticks required for the tests
    tick_bitmap: dict[int, UniswapV3BitmapAtWord] = {}
    for tick in range(MIN_TICK, MAX_TICK, tick_spacing):
        word_pos, _ = position(tick=tick)
        if not tick_bitmap.get(word_pos):
            tick_bitmap[word_pos] = UniswapV3BitmapAtWord()
    for tick in initialized_ticks:
        flip_tick(tick_bitmap=tick_bitmap, tick=tick, tick_spacing=1)

    # lte = false tests

    # returns tick to right if at initialized tick
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (84, True)

    # returns tick to right if at initialized tick
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=-55,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (-4, True)

    # returns the tick directly to the right
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=77,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (78, True)

    # returns the tick directly to the right
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=-56,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (-55, True)

    # returns the next words initialized tick if on the right boundary
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (511, False)

    # returns the next words initialized tick if on the right boundary
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (-200, True)

    # does not exceed boundary
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=508,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (511, False)

    # skips entire word
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (511, False)

    # skips half word
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=383,
        tick_spacing=tick_spacing,
        less_than_or_equal=False,
    ) == (511, False)

    # lte = true tests

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (78, True)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=79,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (78, True)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=258,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (256, False)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=256,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (256, False)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=72,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (70, True)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (-512, False)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=1023,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (768, False)

    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (768, False)

    flip_tick(tick_bitmap=tick_bitmap, tick=329, tick_spacing=1)
    assert next_initialized_tick_within_one_word(
        tick_bitmap=tick_bitmap,
        tick=456,
        tick_spacing=tick_spacing,
        less_than_or_equal=True,
    ) == (329, True)
