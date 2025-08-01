import bisect
from collections.abc import Generator
from itertools import count

from pydantic import SkipValidation, validate_call

from degenbot.constants import MAX_UINT8
from degenbot.exceptions.liquidity_pool import LiquidityMapWordMissing
from degenbot.functions import evm_divide
from degenbot.types.aliases import BlockNumber
from degenbot.uniswap.v3_libraries.bit_math import least_significant_bit, most_significant_bit
from degenbot.uniswap.v3_libraries.tick_math import ValidatedTick
from degenbot.uniswap.v3_types import InitializedTickMap, LiquidityMap, Tick, UniswapV3BitmapAtWord
from degenbot.validation.evm_values import ValidatedInt16, ValidatedInt24

# NOTE: Pydantic validation is applied to certain functions to enforce the built-in integer range
# guarantees from the Solidity contract. Pydantic's validation will copy mutable arguments when
# validating, which defeats the in-place mutation performed by certain functions. The
# `SkipValidation` type is applied so the original dict/list is referenced.


@validate_call
def flip_tick(
    *,
    tick_bitmap: SkipValidation[InitializedTickMap],
    sparse: bool,
    tick: ValidatedTick,
    tick_spacing: ValidatedInt24,
    update_block: BlockNumber,
) -> None:
    """
    Flips the initialized state for a given tick from false to true, or vice versa
    """

    assert tick % tick_spacing == 0, "Invalid tick or spacing"

    word_pos, bit_pos = position(evm_divide(tick, tick_spacing))

    if word_pos not in tick_bitmap:
        if sparse:
            raise LiquidityMapWordMissing(word_pos)
        tick_bitmap[word_pos] = UniswapV3BitmapAtWord(
            bitmap=0,
            block=update_block,
        )

    current_bitmap = tick_bitmap[word_pos]
    new_bitmap = UniswapV3BitmapAtWord(
        bitmap=current_bitmap.bitmap ^ (1 << bit_pos),
        block=update_block,
    )
    tick_bitmap[word_pos] = new_bitmap
    assert tick_bitmap[word_pos] is new_bitmap


@validate_call(validate_return=True)
def next_initialized_tick_within_one_word_legacy(
    *,
    tick_bitmap: SkipValidation[dict[int, UniswapV3BitmapAtWord]],
    tick: ValidatedTick,
    tick_spacing: ValidatedInt24,
    less_than_or_equal: bool,
) -> tuple[ValidatedInt24, bool]:
    """
    Returns the next initialized tick contained in the same word (or adjacent word) as the tick that
    is either to the left (less than or equal to) or right (greater than) of the given tick.
    """

    # Python rounds down to negative infinity, so use it directly instead of the abs and modulo
    # implementation of the Solidity contract
    compressed = tick // tick_spacing

    if less_than_or_equal:
        word_pos, bit_pos = position(compressed)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        bitmap_at_word = tick_bitmap[word_pos].bitmap
        mask = 2 * (1 << bit_pos) - 1  # all the 1s at or to the right of the current bitPos
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the right of or at the current tick, return rightmost
        # in the word
        initialized_status = masked != 0
        next_tick = (
            (compressed - (bit_pos - most_significant_bit(masked))) * tick_spacing
            if initialized_status
            else (compressed - bit_pos) * tick_spacing
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, bit_pos = position(compressed + 1)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        bitmap_at_word = tick_bitmap[word_pos].bitmap
        mask = ~((1 << bit_pos) - 1)  # all the 1s at or to the left of the bitPos
        masked = bitmap_at_word & mask

        # If there are no initialized ticks to the left of the current tick, return leftmost in the
        # word
        initialized_status = masked != 0
        next_tick = (
            (compressed + 1 + (least_significant_bit(masked) - bit_pos)) * tick_spacing
            if initialized_status
            else (compressed + 1 + (MAX_UINT8 - bit_pos)) * tick_spacing
        )

    return next_tick, initialized_status


@validate_call
def gen_ticks(
    *,
    tick_data: SkipValidation[LiquidityMap],
    starting_tick: ValidatedTick,
    tick_spacing: ValidatedInt24,
    less_than_or_equal: bool,
) -> Generator[tuple[Tick, bool], None, None]:
    """
    Yields ticks from the set of all possible ticks at 32 byte (256 bit) word boundaries and
    initialized ticks found in the liquidity mapping. The ticks are yielded in descending order when
    `less_than_or_equal` is True, else ascending.
    """

    # Python rounds down to negative infinity, so use it directly instead of the abs and modulo
    # implementation of the Solidity contract
    compressed = starting_tick // tick_spacing
    word_pos, _ = position(compressed)

    # The boundary ticks for each word are at the 0th and 255th bits.
    # On the way down (less_than_or_equal=True), start at the 0th bit.
    # On the way up (less_than_or_equal=False), start at the 255th bit.
    if less_than_or_equal:
        step_distance = -256 * tick_spacing
        first_boundary_tick = tick_spacing * 256 * word_pos
    else:
        step_distance = 256 * tick_spacing
        first_boundary_tick = tick_spacing * (256 * word_pos + 255)
        if starting_tick >= first_boundary_tick:
            # Special case: starting tick on the first word boundary, begin at the next word
            first_boundary_tick += 256 * tick_spacing
    boundary_ticks_iter = count(
        start=first_boundary_tick,
        step=step_distance,
    )

    # All initialized ticks are known from the tick_data mapping.
    initialized_ticks_iter = iter(
        sorted((tick for tick in tick_data if tick <= starting_tick), reverse=True)
        if less_than_or_equal
        else sorted(tick for tick in tick_data if tick > starting_tick)
    )

    next_initialized_tick = next(initialized_ticks_iter, None)
    next_boundary_tick = next(boundary_ticks_iter)

    if less_than_or_equal:
        # Yield the greater of the nearest initialized and boundary tick until the initialized ticks
        # are exhausted
        while next_initialized_tick is not None:
            if next_initialized_tick > next_boundary_tick:
                yield (next_initialized_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
            elif next_boundary_tick > next_initialized_tick:
                yield (next_boundary_tick, False)
                next_boundary_tick = next(boundary_ticks_iter)
            else:
                # The next initialized tick lies on a boundary, so advance both iterators
                yield (next_boundary_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
                next_boundary_tick = next(boundary_ticks_iter)
    else:
        # Yield the lesser of the nearest initialized and boundary tick until the initialized ticks
        # are exhausted
        while next_initialized_tick is not None:
            if next_initialized_tick < next_boundary_tick:
                yield (next_initialized_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
            elif next_boundary_tick < next_initialized_tick:
                yield (next_boundary_tick, False)
                next_boundary_tick = next(boundary_ticks_iter)
            else:
                # The next initialized tick lies on a boundary, so advance both iterators
                yield (next_boundary_tick, True)
                next_initialized_tick = next(initialized_ticks_iter, None)
                next_boundary_tick = next(boundary_ticks_iter)

    # Then yield uninitialized boundary ticks forever
    while True:
        yield (next_boundary_tick, False)
        next_boundary_tick = next(boundary_ticks_iter)


@validate_call(validate_return=True)
def next_initialized_tick_within_one_word(
    *,
    tick_bitmap: SkipValidation[InitializedTickMap],
    tick_data: SkipValidation[LiquidityMap],
    tick: ValidatedTick,
    tick_spacing: ValidatedInt24,
    less_than_or_equal: bool,
) -> tuple[ValidatedInt24, bool]:
    """
    Returns the next initialized tick contained in the same word (or adjacent word) as the tick that
    is either to the left (less than or equal to) or right (greater than) of the given tick.
    """

    compressed = tick // tick_spacing

    if less_than_or_equal:
        if tick in tick_data:
            return tick, True

        word_pos, _ = position(compressed)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        lowest_tick_in_word = tick_spacing * (256 * word_pos)
        known_ticks = sorted(tick_data)
        tick_index = bisect.bisect_left(known_ticks, tick)

        next_tick = (
            lowest_tick_in_word
            if tick_index == 0
            else max(lowest_tick_in_word, known_ticks[tick_index - 1])
        )
    else:
        # start from the word of the next tick, since the current tick state doesn't matter
        word_pos, _ = position(compressed + 1)

        if word_pos not in tick_bitmap:
            raise LiquidityMapWordMissing(word_pos)

        highest_tick_in_word = tick_spacing * (256 * word_pos) + tick_spacing * 255
        known_ticks = sorted(tick_data)
        tick_index = bisect.bisect_right(known_ticks, tick)

        next_tick = (
            highest_tick_in_word
            if tick_index == len(known_ticks)
            else min(highest_tick_in_word, known_ticks[tick_index])
        )

    return next_tick, next_tick in tick_data


@validate_call(validate_return=True)
def position(tick: ValidatedInt24) -> tuple[ValidatedInt16, ValidatedInt16]:
    """
    Computes the position in the mapping where the initialized bit for a tick is placed
    """

    return (
        tick >> 8,  # word_pos
        tick % 256,  # bit_pos
    )
