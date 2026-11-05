"""Stable companion home for Curve StableSwap math functions.

Re-exports the Rust-backed math from the ``degenbot._ffi.curve_math``
submodule with un-prefixed names. Importers should use::

    from degenbot.curve.math import stableswap_get_y

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot_math::curve``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-math``
crate's ``curve`` module; see that module's docs for math semantics.
"""

from degenbot._ffi.curve_math import (
    derive_rate_and_precision_multipliers,
    stableswap_get_d,
    stableswap_get_y,
    stableswap_get_y_d,
    stableswap_newton_y,
    stableswap_reduction_coefficient,
)

__all__ = [
    "derive_rate_and_precision_multipliers",
    "stableswap_get_d",
    "stableswap_get_y",
    "stableswap_get_y_d",
    "stableswap_newton_y",
    "stableswap_reduction_coefficient",
]
