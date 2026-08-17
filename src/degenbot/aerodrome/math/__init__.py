"""Stable companion home for Solidly/Aerodrome/Camelot math functions.

Re-exports the Rust-backed math from the ``degenbot._ffi.solidly_math``
submodule. Importers should use::

    from degenbot.aerodrome.math import calc_exact_in_volatile

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot-solidly-math`` with ``solidly.rs`` + ``camelot.rs``) show
through to Python.

The `solidly_` prefix was dropped on the submodule (it was a flat-root
artifact); `camelot_` stays because camelot is a distinct variant in the
same crate.

The functions are thin PyO3 wrappers over the pure-Rust
``degenbot-solidly-math`` core crate; see that crate's docs for math
semantics.
The volatile V2 exact-out (`calc_exact_out_v2`) comes from the
`degenbot-v2-math` core crate via `degenbot._ffi.v2_math`.
"""

from degenbot._ffi.solidly_math import (
    calc_d,
    calc_exact_in_stable_camelot,
    calc_exact_in_stable_solidly,
    calc_exact_in_volatile,
    calc_exact_out_stable_solidly,
    calc_f,
    calc_k,
    camelot_f,
    camelot_get_y_camelot,
    camelot_k,
    get_y_solidly,
)
from degenbot._ffi.v2_math import calc_exact_out_v2

__all__ = [
    "calc_d",
    "calc_exact_in_stable_camelot",
    "calc_exact_in_stable_solidly",
    "calc_exact_in_volatile",
    "calc_exact_out_stable_solidly",
    "calc_exact_out_v2",
    "calc_f",
    "calc_k",
    "camelot_f",
    "camelot_get_y_camelot",
    "camelot_k",
    "get_y_solidly",
]
