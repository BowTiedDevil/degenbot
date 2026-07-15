"""Uniswap V3 library functions.

Thin delegation shims over the Rust ``degenbot-cl-math`` core, exposed via
the ``degenbot_rs`` extension. Each submodule adds only Solidity-matching
input validation, ``lru_cache`` memoization, and ``EVMRevertError`` conversion.
"""

from degenbot._ffi import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)
from degenbot._ffi import (
    cl_add_delta as add_delta,
)
from degenbot._ffi import (
    cl_compute_swap_step_v3 as compute_swap_step,
)
from degenbot._ffi import (
    cl_div_rounding_up as div_rounding_up,
)
from degenbot._ffi import (
    cl_get_amount0_delta as get_amount0_delta,
)
from degenbot._ffi import (
    cl_get_amount1_delta as get_amount1_delta,
)
from degenbot._ffi import (
    cl_get_next_sqrt_price_from_amount0_rounding_up as get_next_sqrt_price_from_amount0_rounding_up,
)
from degenbot._ffi import (
    cl_get_next_sqrt_price_from_amount1_rounding_down as get_next_sqrt_price_from_amount1_rounding_down,  # noqa: E501
)
from degenbot._ffi import (
    cl_get_next_sqrt_price_from_input as get_next_sqrt_price_from_input,
)
from degenbot._ffi import (
    cl_get_next_sqrt_price_from_output as get_next_sqrt_price_from_output,
)
from degenbot._ffi import (
    cl_max_usable_tick as max_usable_tick,
)
from degenbot._ffi import (
    cl_min_usable_tick as min_usable_tick,
)

# `muldiv`/`muldiv_rounding_up`/`least_significant_bit`/`most_significant_bit`
# are NOT re-exported here: the wrapped companions in `.full_math` / `.bit_math`
# are the canonical Solidity-matching surface (they raise `EVMRevertError`.
# The raw `cl_*` re-exports that previously lived here collided by name and had
# zero package-level consumers — consumers reach the leaf submodule directly.

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
    "max_usable_tick",
    "min_usable_tick",
]
