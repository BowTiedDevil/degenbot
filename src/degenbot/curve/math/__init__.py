"""Stable companion home for Curve StableSwap math functions.

Re-exports the Rust-backed math from the ``degenbot._ffi.curve_math``
submodule with un-prefixed names. Importers should use::

    from degenbot.curve.math import stableswap_get_y

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot-curve-math``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust
``degenbot-curve-math`` core crate; see that crate's docs for math semantics.
"""

from degenbot._ffi.curve_math import (
    stableswap_get_d,
    stableswap_get_y,
    stableswap_get_y_d,
    stableswap_newton_y,
    stableswap_reduction_coefficient,
)

__all__ = [
    "stableswap_get_d",
    "stableswap_get_y",
    "stableswap_get_y_d",
    "stableswap_newton_y",
    "stableswap_reduction_coefficient",
]
