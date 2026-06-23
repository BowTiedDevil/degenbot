"""Uniswap V3 library functions.

Thin delegation shims over the Rust ``degenbot-cl-math`` core, exposed via
the ``degenbot_rs`` extension. Each submodule adds only Solidity-matching
input validation, ``lru_cache`` memoization, and ``EVMRevertError`` conversion.
"""

from degenbot.degenbot_rs import (
    cl_add_delta as add_delta,
)
from degenbot.degenbot_rs import (
    cl_compute_swap_step_v3 as compute_swap_step,
)
from degenbot.degenbot_rs import (
    cl_div_rounding_up as div_rounding_up,
)
from degenbot.degenbot_rs import (
    cl_get_amount0_delta as get_amount0_delta,
)
from degenbot.degenbot_rs import (
    cl_get_amount1_delta as get_amount1_delta,
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_amount0_rounding_up as get_next_sqrt_price_from_amount0_rounding_up,
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_amount1_rounding_down as get_next_sqrt_price_from_amount1_rounding_down,  # noqa: E501
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_input as get_next_sqrt_price_from_input,
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_output as get_next_sqrt_price_from_output,
)
from degenbot.degenbot_rs import (
    cl_least_significant_bit as least_significant_bit,
)
from degenbot.degenbot_rs import (
    cl_max_usable_tick as max_usable_tick,
)
from degenbot.degenbot_rs import (
    cl_min_usable_tick as min_usable_tick,
)
from degenbot.degenbot_rs import (
    cl_most_significant_bit as most_significant_bit,
)
from degenbot.degenbot_rs import (
    cl_muldiv as muldiv,
)
from degenbot.degenbot_rs import (
    cl_muldiv_rounding_up as muldiv_rounding_up,
)
from degenbot.degenbot_rs import (
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
