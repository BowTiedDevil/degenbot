from degenbot.uniswap.v3.libraries import TickBitmap, TickMath
from degenbot.uniswap.v3.v3_liquidity_pool import UniswapV3BitmapAtWord


def test_tickbitmap():
    ### ----------------------------------------------------
    ### TickBitmap tests
    ### ----------------------------------------------------

    # nextInitializedTickWithinOneWord tests

    # set up a full-sized tickBitmap with empty strings for all possible words
    tickBitmap = {}
    for tick in range(TickMath.MIN_TICK, TickMath.MAX_TICK):
        # for tick in range(-2, 2):
        wordPos, _ = TickBitmap.position(tick=tick)
        if not tickBitmap.get(wordPos):
            # tickBitmap[wordPos] = {"bitmap": 0, "block": None}
            tickBitmap[wordPos] = UniswapV3BitmapAtWord(bitmap=0, block=None)

    # flip the specified bits as initialized
    for tick in [-200, -55, -4, 70, 78, 84, 139, 240, 535]:
        TickBitmap.flipTick(tickBitmap=tickBitmap, tick=tick, tickSpacing=1)

    # lte = false tests

    # returns tick to right if at initialized tick
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=78, tickSpacing=1, lte=False
    )
    assert next == 84
    assert initialized == True

    # returns tick to right if at initialized tick
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=-55, tickSpacing=1, lte=False
    )
    assert next == -4
    assert initialized == True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=77, tickSpacing=1, lte=False
    )
    assert next == 78
    assert initialized == True

    # returns the tick directly to the right
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=-56, tickSpacing=1, lte=False
    )
    assert next == -55
    assert initialized == True

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=255, tickSpacing=1, lte=False
    )
    assert next == 511
    assert initialized == False

    # returns the next words initialized tick if on the right boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=-257, tickSpacing=1, lte=False
    )
    assert next == -200
    assert initialized == True

    # does not exceed boundary
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=508, tickSpacing=1, lte=False
    )
    assert next == 511
    assert initialized == False

    # skips entire word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=255, tickSpacing=1, lte=False
    )
    assert next == 511
    assert initialized == False

    # skips half word
    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=383, tickSpacing=1, lte=False
    )
    assert next == 511
    assert initialized == False

    # lte = true tests

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=78, tickSpacing=1, lte=True
    )
    assert next == 78
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=79, tickSpacing=1, lte=True
    )
    assert next == 78
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=258, tickSpacing=1, lte=True
    )
    assert next == 256
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=256, tickSpacing=1, lte=True
    )
    assert next == 256
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=72, tickSpacing=1, lte=True
    )
    assert next == 70
    assert initialized == True

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=-257, tickSpacing=1, lte=True
    )
    assert next == -512
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=1023, tickSpacing=1, lte=True
    )
    assert next == 768
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=900, tickSpacing=1, lte=True
    )
    assert next == 768
    assert initialized == False

    next, initialized = TickBitmap.nextInitializedTickWithinOneWord(
        tickBitmap=tickBitmap, tick=900, tickSpacing=1, lte=True
    )
