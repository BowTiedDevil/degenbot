"""Uniswap V3 library functions.

Stand-alone tick-math primitives re-exported from the Rust
``degenbot-concentrated-liquidity-math`` core. These are the **stateless pure conversions**
(tick ↔ √P and the CL boundary constants) that a driver or pool class
needs without holding any pool state — e.g. to set a price-limit by tick.

The CL swap-step machinery (``get_next_sqrt_price_from_*``,
``add_delta``, ``get_amount0/1_delta``, ``max/min_usable_tick``,
``div_rounding_up``, ``simple_mul_div``,
``apply_liquidity_mapping_update``) is the guts of the CL swap-step loop,
which the Rust core owns and runs end-to-end inside ``LiquidityPool``. The
U64B2O census found no Python consumer, so those internals are removed from
the FFI surface entirely (``compute_swap_step_v3``/``_v4`` remain as the
dual-driver parity fixture seam).

The leaf submodules (``.full_math``, ``.bit_math``, ``.tick``,
``.tick_bitmap``, ``.functions``) are the Solidity-matching surface that
adds input validation and ``EVMRevertError`` conversion on top of the
``cl_*`` primitives.
"""

from degenbot.uniswap.math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
]
