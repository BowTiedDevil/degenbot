"""Stable companion home for Balancer V2 math functions.

Re-exports the Rust-backed math from the ``degenbot._ffi.balancer_math``
submodule with un-prefixed names. Importers should use::

    from degenbot.balancer.math import fixed_point_div_down

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and the un-prefixed names let the Rust
crate structure (``degenbot-balancer-math`` → ``fixed_point`` /
``weighted_math`` / ``stable_math``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-balancer-math``
core crate; see that crate's docs for the math semantics.
"""

from degenbot._ffi.balancer_math import (
    fixed_point_div_down,
    fixed_point_div_up,
    fixed_point_mul_down,
    stable_calc_in_given_out,
    stable_calc_out_given_in,
    stable_calculate_invariant,
    stable_calculate_invariant_deployed,
    weighted_add_swap_fee_amount,
    weighted_calc_in_given_out,
    weighted_calc_out_given_in,
    weighted_calculate_invariant,
    weighted_subtract_swap_fee_amount,
)

__all__ = [
    "fixed_point_div_down",
    "fixed_point_div_up",
    "fixed_point_mul_down",
    "stable_calc_in_given_out",
    "stable_calc_out_given_in",
    "stable_calculate_invariant",
    "stable_calculate_invariant_deployed",
    "weighted_add_swap_fee_amount",
    "weighted_calc_in_given_out",
    "weighted_calc_out_given_in",
    "weighted_calculate_invariant",
    "weighted_subtract_swap_fee_amount",
]
