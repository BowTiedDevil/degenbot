"""Stable companion home for concentrated-liquidity math functions.

Re-exports the Rust-backed math from the ``degenbot._ffi.concentrated_liquidity_math`` submodule
with un-prefixed names. Importers should use::

    from degenbot.uniswap.math import muldiv, compute_swap_step_v3

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot-concentrated-liquidity-math``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-concentrated-liquidity-math``
core crate. The CL math is shared across Uniswap V3 and V4, so this companion
lives at ``degenbot.uniswap.math`` (variant-neutral), not under
``v3_libraries``.
"""

from degenbot._ffi.concentrated_liquidity_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    add_delta,
    apply_liquidity_mapping_update,
    compute_swap_step_v3,
    compute_swap_step_v4,
    div_rounding_up,
    get_amount0_delta,
    get_amount1_delta,
    get_next_sqrt_price_from_amount0_rounding_up,
    get_next_sqrt_price_from_amount1_rounding_down,
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
    get_tick_word_and_bit_position,
    least_significant_bit,
    max_usable_tick,
    min_usable_tick,
    most_significant_bit,
    muldiv,
    muldiv_rounding_up,
    simple_mul_div,
)

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "add_delta",
    "apply_liquidity_mapping_update",
    "compute_swap_step_v3",
    "compute_swap_step_v4",
    "div_rounding_up",
    "get_amount0_delta",
    "get_amount1_delta",
    "get_next_sqrt_price_from_amount0_rounding_up",
    "get_next_sqrt_price_from_amount1_rounding_down",
    "get_next_sqrt_price_from_input",
    "get_next_sqrt_price_from_output",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "get_tick_word_and_bit_position",
    "least_significant_bit",
    "max_usable_tick",
    "min_usable_tick",
    "most_significant_bit",
    "muldiv",
    "muldiv_rounding_up",
    "simple_mul_div",
]
