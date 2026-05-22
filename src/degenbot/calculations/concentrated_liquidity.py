"""Concentrated liquidity (Uniswap V3/V4) tick math.

This module is intentionally minimal — the bulk of concentrated liquidity math
already lives in well-organized DEX-specific libraries:

- ``degenbot.uniswap.v3_libraries/`` — tick_math, full_math, sqrt_price_math, swap_math
- ``degenbot.uniswap.v4_libraries/`` — same for V4

Those libraries are standalone pure functions and don't need to move here.
This module exists for any cross-DEX shared utilities that emerge.
"""

from __future__ import annotations

from dataclasses import dataclass

from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_libraries.tick_bitmap import flip_tick


@dataclass(frozen=True, slots=True)
class LiquidityMappingUpdateResult:
    """Result of applying a liquidity mapping update to tick bitmap and tick data."""

    tick_bitmap: dict[int, BitmapAtWord]
    tick_data: dict[int, LiquidityAtTick]
    liquidity: int


def apply_liquidity_mapping_update(
    *,
    tick_bitmap: dict[int, BitmapAtWord],
    tick_data: dict[int, LiquidityAtTick],
    tick_spacing: int,
    tick: int,
    liquidity: int,
    initial_state_block: int,
    update_block: int,
    tick_lower: int,
    tick_upper: int,
    liquidity_delta: int,
) -> LiquidityMappingUpdateResult:
    """Apply a liquidity mapping update to tick bitmap and tick data dicts.

    This is the pure computation core extracted from
    ``UniswapV3Pool.update_liquidity_map`` and ``UniswapV4Pool.update_liquidity_map``.

    The caller is responsible for:
    - Providing complete (non-sparse) tick maps
    - Threading/locking if needed
    - State management (pushing the new state)
    - Notifying subscribers

    The dicts are copied; the originals are not mutated.
    """
    state_block = update_block

    # Work on copies so the originals are not mutated
    working_tick_bitmap = dict(tick_bitmap)
    working_tick_data = dict(tick_data)
    working_liquidity = liquidity

    assert working_liquidity >= 0, (
        f"Starting liquidity violates invariant: tick={tick} liquidity={liquidity}"
    )

    # Adjust in-range liquidity if the modified region includes the active tick.
    if tick_lower <= tick < tick_upper and state_block > initial_state_block:
        working_liquidity += liquidity_delta
        assert working_liquidity >= 0, (
            f"In-range liquidity adjustment violated invariant: "
            f"tick={tick} liquidity={liquidity} update_block={update_block} "
            f"liquidity_delta={liquidity_delta}"
        )

    for current_tick in (tick_lower, tick_upper):
        tick_word, _ = get_tick_word_and_bit_position(current_tick, tick_spacing)

        if tick_word not in working_tick_bitmap:
            # For non-sparse maps (as used by the CLI), create an empty entry
            # so flip_tick can work
            working_tick_bitmap[tick_word] = BitmapAtWord(bitmap=0, block=state_block)

        # Get the liquidity info for this tick. If the mapping is empty at this tick, it is
        # uninitialized and must be flipped in the bitmap and initialized as empty in the
        # mapping
        if current_tick not in working_tick_data:
            working_tick_data[current_tick] = LiquidityAtTick(
                liquidity_net=0,
                liquidity_gross=0,
                block=state_block,
            )
            flip_tick(
                tick_bitmap=working_tick_bitmap,
                sparse=False,
                tick=current_tick,
                tick_spacing=tick_spacing,
                update_block=state_block,
            )

        current_liquidity_net = working_tick_data[current_tick].liquidity_net
        current_liquidity_gross = working_tick_data[current_tick].liquidity_gross

        new_liquidity_gross = current_liquidity_gross + liquidity_delta
        assert new_liquidity_gross >= 0, "Negative gross liquidity!"

        if new_liquidity_gross == 0:
            # Delete tick from the map if there is no remaining liquidity referencing it,
            # and flip it in the bitmap
            del working_tick_data[current_tick]
            flip_tick(
                tick_bitmap=working_tick_bitmap,
                sparse=False,
                tick=current_tick,
                tick_spacing=tick_spacing,
                update_block=state_block,
            )
            continue

        # Liquidity positions include the lower tick, but exclude the upper tick.
        if current_tick == tick_lower:
            new_liquidity_net = current_liquidity_net + liquidity_delta
        else:
            new_liquidity_net = current_liquidity_net - liquidity_delta

        working_tick_data[current_tick] = LiquidityAtTick(
            liquidity_net=new_liquidity_net,
            liquidity_gross=new_liquidity_gross,
            block=state_block,
        )

    return LiquidityMappingUpdateResult(
        tick_bitmap=working_tick_bitmap,
        tick_data=working_tick_data,
        liquidity=working_liquidity,
    )
