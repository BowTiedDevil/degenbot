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
    compute_swap_step_v3,
    compute_swap_step_v4,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
    get_tick_word_and_bit_position,
    least_significant_bit,
    most_significant_bit,
    muldiv,
    muldiv_rounding_up,
)
from degenbot._ffi.v2_math import calc_exact_in_v2, calc_exact_out_v2


__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "calc_exact_in_v2",
    "calc_exact_out_v2",
    "compute_swap_step_v3",
    "compute_swap_step_v4",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "get_tick_word_and_bit_position",
    "least_significant_bit",
    "most_significant_bit",
    "muldiv",
    "muldiv_rounding_up",
]
