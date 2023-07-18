from degenbot.uniswap.v3.libraries import TickBitmap, TickMath
from degenbot.uniswap.v3.v3_liquidity_pool import UniswapV3BitmapAtWord
from typing import Dict

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/TickBitmap.spec.ts


def is_initialized(tick_bitmap: Dict[int, UniswapV3BitmapAtWord], tick: int):
    # Adapted from Uniswap test contract
    # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/test/TickBitmapTest.sol

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap, tick, 1, True
    )
    return next == tick if initialized else False


def empty_bitmap():
    """
    Generates an empty tick bitmap of maximum size
    """

    tick_bitmap = {}
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK):
        wordPos, _ = TickBitmap.position(tick=tick)
        tick_bitmap[wordPos] = UniswapV3BitmapAtWord()
    return tick_bitmap


def test_isInitialized():
    tick_bitmap = empty_bitmap()

    assert is_initialized(tick_bitmap, 1) == False

    TickBitmap.flipTick(tick_bitmap, 1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) == True

    # TODO: The repo flips this tick twice, which may be a mistake
    # TickBitmap.flipTick(tick_bitmap, 1, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=1, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) == False

    TickBitmap.flipTick(tick_bitmap, tick=2, tick_spacing=1)
    assert is_initialized(tick_bitmap, 1) == False

    TickBitmap.flipTick(tick_bitmap, tick=1 + 256, tick_spacing=1)
    assert is_initialized(tick_bitmap, 257) == True
    assert is_initialized(tick_bitmap, 1) == False


def test_flipTick():
    tick_bitmap = empty_bitmap()

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) == True
    assert is_initialized(tick_bitmap, -231) == False
    assert is_initialized(tick_bitmap, -229) == False
    assert is_initialized(tick_bitmap, -230 + 256) == False
    assert is_initialized(tick_bitmap, -230 - 256) == False

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    assert is_initialized(tick_bitmap, -230) == False
    assert is_initialized(tick_bitmap, -231) == False
    assert is_initialized(tick_bitmap, -229) == False
    assert is_initialized(tick_bitmap, -230 + 256) == False
    assert is_initialized(tick_bitmap, -230 - 256) == False

    TickBitmap.flipTick(tick_bitmap, tick=-230, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-229, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=500, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-229, tick_spacing=1)
    TickBitmap.flipTick(tick_bitmap, tick=-259, tick_spacing=1)

    assert is_initialized(tick_bitmap, -259) == True
    assert is_initialized(tick_bitmap, -229) == False


def test_nextInitializedTickWithinOneWord():
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
    assert initialized == True

    # returns tick to right if at initialized tick
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-55,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -4
    assert initialized == True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=77,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 78
    assert initialized == True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-56,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -55
    assert initialized == True

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized == False

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == -200
    assert initialized == True

    # does not exceed boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=508,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized == False

    # skips entire word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=255,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized == False

    # skips half word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=383,
        tick_spacing=1,
        less_than_or_equal=False,
    )
    assert next == 511
    assert initialized == False

    # lte = true tests

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=78,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 78
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=79,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 78
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=258,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 256
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=256,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 256
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=72,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 70
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=-257,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == -512
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=1023,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 768
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=1,
        less_than_or_equal=True,
    )
    assert next == 768
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tick_bitmap=tick_bitmap,
        tick=900,
        tick_spacing=1,
        less_than_or_equal=True,
    )
