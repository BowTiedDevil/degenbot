"""Curve StableSwap invariant calculations.

Pure functions implementing the Curve V1 StableSwap invariant D and Y solvers
and their variant forms.

These were originally nested closures inside CurveStableswapPool._get_d().
Plan 029 (Variant Group Externalization) made the variant selection explicit via
DVariant/YVariant/YDVariant enums. This module extracts the actual formula functions
so they can be tested independently and reused by other StableSwap implementations
(e.g., Aerodrome stable pools).

All functions are pure: numeric inputs → numeric outputs, no self, no class references.
"""

from collections.abc import Sequence

from degenbot.exceptions.evm import EVMRevertError


# ── D calculation variants ──
# Each computes a single Newton step: d_new = d_func(d, d_p, s, a_nn, n_coins, a_precision)


def calc_d(
    *,
    a_nn: int,
    s: int,
    d: int,
    d_p: int,
    n_coins: int,
    a_precision: int,
) -> int:
    """Standard D step: divides by (a_nn - a_precision) * d / a_precision."""
    return (
        (a_nn * s // a_precision + d_p * n_coins)
        * d
        // ((a_nn - a_precision) * d // a_precision + (n_coins + 1) * d_p)
    )


def calc_d_variant_alpha(
    *,
    a_nn: int,
    s: int,
    d: int,
    d_p: int,
    n_coins: int,
    a_precision: int,  # noqa: ARG001 — unused in this variant
) -> int:
    """Variant alpha D step: omits a_precision from the formula entirely."""
    return (a_nn * s + d_p * n_coins) * d // ((a_nn - 1) * d + (n_coins + 1) * d_p)


# ── D' (d_prev) calculation variants ──
# Each computes d_p from the current D and xp values


def calc_dp(
    *,
    d: int,
    d_p: int,
    xp: Sequence[int],
    n_coins: int,
) -> int:
    """Standard D' step: d_p = d_p * d // (x * n_coins) for each x in xp."""
    for x in xp:
        d_p = d_p * d // (x * n_coins)
    return d_p


def calc_dp_variant_alpha(
    *,
    d: int,
    d_p: int,
    xp: Sequence[int],
    n_coins: int,
) -> int:
    """Variant alpha D' step: adds +1 to denominator (d_p * d // (x * n_coins + 1))."""
    for x in xp:
        d_p = d_p * d // (x * n_coins + 1)
    return d_p


def calc_dp_variant_beta(
    *,
    d: int,
    d_p: int,  # noqa: ARG001 — unused in this variant
    xp: Sequence[int],
    n_coins: int,
) -> int:
    """Variant beta D' step: uses only first two coins, no loop (d*d/x0 * d/x1 / n^2)."""
    return d * d // xp[0] * d // xp[1] // n_coins**2


def calc_dp_variant_gamma(
    *,
    d: int,
    d_p: int,  # noqa: ARG001 — unused in this variant
    xp: Sequence[int],
    n_coins: int,
) -> int:
    """Variant gamma D' step: like beta but uses n^n instead of n^2."""
    return d * d // xp[0] * d // xp[1] // n_coins**n_coins
