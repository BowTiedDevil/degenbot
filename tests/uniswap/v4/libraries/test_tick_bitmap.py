import hypothesis
import hypothesis.strategies

from degenbot.constants import MAX_INT24, MIN_INT24
from degenbot.uniswap.v4_libraries.tick_bitmap import compress, position

# Tests adapted from Foundry tests in the Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/TickBitmap.t.sol


@hypothesis.given(
    tick=hypothesis.strategies.integers(
        min_value=MIN_INT24,
        max_value=MAX_INT24,
    ),
    tick_spacing=hypothesis.strategies.integers(
        min_value=MIN_INT24,
        max_value=MAX_INT24,
    ),
)
def test_fuzz_compress(tick: int, tick_spacing: int):
    hypothesis.assume(tick_spacing >= 1)

    # emulate the EVM division behavior, which floors toward zero
    compressed = -(-tick // tick_spacing) if tick < 0 else tick // tick_spacing
    if tick < 0 and tick % tick_spacing != 0:
        compressed -= 1

    assert compress(tick, tick_spacing) == compressed


@hypothesis.given(
    tick=hypothesis.strategies.integers(
        min_value=MIN_INT24,
        max_value=MAX_INT24,
    ),
)
def test_fuzz_position(tick: int):
    word_pos, bit_pos = position(tick)
    assert word_pos == tick >> 8
    assert bit_pos == tick % 256
