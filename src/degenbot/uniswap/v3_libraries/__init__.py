"""Uniswap V3 library functions.

Provides Rust-accelerated CL math functions via the ``degenbot_rs`` extension.
Python implementations remain in each submodule for reference and testing.
"""

from degenbot.degenbot_rs import (
    cl_add_delta as add_delta,
    cl_compute_swap_step_v3 as compute_swap_step,
    cl_div_rounding_up as div_rounding_up,
    cl_get_amount0_delta as get_amount0_delta,
    cl_get_amount1_delta as get_amount1_delta,
    cl_get_next_sqrt_price_from_amount0_rounding_up as get_next_sqrt_price_from_amount0_rounding_up,
    cl_get_next_sqrt_price_from_amount1_rounding_down as get_next_sqrt_price_from_amount1_rounding_down,
    cl_get_next_sqrt_price_from_input as get_next_sqrt_price_from_input,
    cl_get_next_sqrt_price_from_output as get_next_sqrt_price_from_output,
    cl_least_significant_bit as least_significant_bit,
    cl_max_usable_tick as max_usable_tick,
    cl_min_usable_tick as min_usable_tick,
    cl_most_significant_bit as most_significant_bit,
    cl_muldiv as muldiv,
    cl_muldiv_rounding_up as muldiv_rounding_up,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)

from .tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
)

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "add_delta",
    "compute_swap_step",
    "div_rounding_up",
    "get_amount0_delta",
    "get_amount1_delta",
    "get_next_sqrt_price_from_amount0_rounding_up",
    "get_next_sqrt_price_from_amount1_rounding_down",
    "get_next_sqrt_price_from_input",
    "get_next_sqrt_price_from_output",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "least_significant_bit",
    "max_usable_tick",
    "min_usable_tick",
    "most_significant_bit",
    "muldiv",
    "muldiv_rounding_up",
]
