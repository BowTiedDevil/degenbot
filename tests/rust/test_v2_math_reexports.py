"""Seam smoke test for the _ffi.v2_math companion re-exports.

The former test_v2_math_parity.py compared the FFI seam against the Python
shadow formulas in degenbot.uniswap.v2_functions; those shadows are now
retired and the numeric vectors live in the owning Rust crate
(degenbot-math, src/v2/mod.rs). The one assertion that was not math
parity - that the companion homes are identity aliases of the FFI functions -
is preserved here.
"""

from __future__ import annotations

from degenbot._ffi.v2_math import calc_exact_in_v2, calc_exact_out_v2


def test_companion_reexports_are_identity_aliases() -> None:
    """The companion homes re-export the exact FFI functions (no wrappers)."""
    from degenbot.aerodrome.math import calc_exact_out_v2 as aero_out
    from degenbot.uniswap.math import calc_exact_in_v2 as uni_in
    from degenbot.uniswap.math import calc_exact_out_v2 as uni_out

    assert aero_out is calc_exact_out_v2
    assert uni_in is calc_exact_in_v2
    assert uni_out is calc_exact_out_v2
