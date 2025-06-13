from degenbot.constants import MAX_UINT128
from degenbot.functions import evm_divide
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK


def tick_spacing_to_max_liquidity_per_tick(tick_spacing: int) -> int:
    min_tick = evm_divide(MIN_TICK, tick_spacing) * tick_spacing
    max_tick = (MAX_TICK // tick_spacing) * tick_spacing
    num_ticks = (max_tick - min_tick) // tick_spacing + 1
    return MAX_UINT128 // num_ticks
