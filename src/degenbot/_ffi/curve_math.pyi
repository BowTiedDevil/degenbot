"""Stub for the dynamically-created ``degenbot._ffi.curve_math`` submodule.

The submodule is created at runtime by ``add_curve_math_module`` in the
PyO3 wrapper crate (``degenbot-python/src/curve_math/lib.rs``). This stub
gives the type checker the function signatures.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-curve-math``
core crate. Vyper reverts (overflow / non-convergence / index / unsafe-value)
surface as ``ValueError``. The variant discriminants (``d_variant`` /
``y_variant`` / ``yd_variant``) are 1-based ``auto()`` enum ``.value`` s
matching the Rust ``try_from_u8``.
"""

def stableswap_get_d(
    xp: list[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    d_variant: int,
) -> int: ...
def stableswap_get_y(
    i: int,
    j: int,
    x: int,
    xp: list[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    y_variant: int,
    d_variant: int,
) -> int: ...
def stableswap_get_y_d(
    amp: int,
    i: int,
    xp: list[int],
    d: int,
    n_coins: int,
    a_precision: int,
    yd_variant: int,
) -> int: ...
def stableswap_newton_y(
    ann: int,
    gamma: int,
    xp: list[int],
    d: int,
    token_index: int,
    n_coins: int,
    a_multiplier: int,
) -> int: ...
def stableswap_reduction_coefficient(x: list[int], fee_gamma: int, n_coins: int) -> int: ...

__all__ = [
    "stableswap_get_d",
    "stableswap_get_y",
    "stableswap_get_y_d",
    "stableswap_newton_y",
    "stableswap_reduction_coefficient",
]
