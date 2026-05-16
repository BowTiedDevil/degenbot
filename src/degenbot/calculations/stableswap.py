"""Curve StableSwap invariant calculations.

Pure functions implementing the Curve V1 StableSwap invariant D and Y solvers
and their variant forms.

Two levels of function:

1. **Step functions** (``calc_d``, ``calc_dp``, etc.) — compute a single Newton step.
   These were originally nested closures inside ``CurveStableswapPool._get_d()``.
   Plan 029 (Variant Group Externalization) made the variant selection explicit via
   DVariant/YVariant/YDVariant enums.

2. **Iterative solvers** (``stableswap_get_d``, ``stableswap_get_y``, etc.) — run
   the Newton iteration to convergence, selecting the step function based on the
   variant enum. These were originally pool methods on ``CurveStableswapPool``.
   Plan 039 (DyCalculator Seam) extracts them as standalone pure functions so that
   calculators can call them without depending on the pool object.

All functions are pure: numeric inputs → numeric outputs, no self, no class references.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.curve.types import DVariant, YDVariant, YVariant

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
    return cast("int", d * d // xp[0] * d // xp[1] // n_coins**n_coins)


# ── Iterative solvers ──
# These run the Newton iteration to convergence, selecting step functions
# based on variant enums. Direct ports of Vyper contract logic.


def stableswap_get_d(
    xp: Sequence[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    d_variant: DVariant,
) -> int:
    """Solve for the Curve StableSwap invariant D using modified Newton's method.

    Direct port of the Vyper contract's D calculation. Iterates until convergence
    (difference ≤ 1) or 255 steps (revert).

    Args:
        xp: Token balances adjusted to equal precision.
        amp: The amplification coefficient A.
        n_coins: Number of coins in the pool.
        a_precision: Precision factor for A (typically 100).
        d_variant: Which D-step / D'-step variant to use.

    Returns:
        The invariant D value.

    Raises:
        ValueError: If D calculation does not converge within 255 iterations,
            or if the sum of xp is zero.
    """
    # Select step functions based on variant
    d_func = calc_d
    dp_func = calc_dp
    match d_variant:
        case d_variant.VARIANT_ALPHA:
            d_func = calc_d_variant_alpha
        case d_variant.VARIANT_ALPHA_DP_ALPHA:
            d_func = calc_d_variant_alpha
            dp_func = calc_dp_variant_alpha
        case d_variant.VARIANT_DP_ALPHA:
            dp_func = calc_dp_variant_alpha
        case d_variant.VARIANT_BETA_DP:
            dp_func = calc_dp_variant_beta
        case d_variant.VARIANT_GAMMA_DP:
            dp_func = calc_dp_variant_gamma
        case d_variant.STANDARD:
            pass

    d: int = sum(xp)
    s: int = d
    if s == 0:
        return 0
    a_nn = amp * n_coins

    for _ in range(255):  # pragma: no branch
        d_p: int = d
        d_prev: int = d
        d_p = dp_func(d=d, d_p=d_p, xp=xp, n_coins=n_coins)
        d = d_func(a_nn=a_nn, s=s, d=d, d_p=d_p, n_coins=n_coins, a_precision=a_precision)
        if d_prev < d:
            if d - d_prev <= 1:
                return d
        elif d_prev - d <= 1:
            return d

    msg = "D calculation did not converge"
    raise ValueError(msg)


def stableswap_get_y(
    i: int,
    j: int,
    *,
    x: int,
    xp: Sequence[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    y_variant: YVariant,
    d_variant: DVariant,
) -> int:
    """Calculate x[j] if one makes x[i] = x in a Curve StableSwap pool.

    Solves the quadratic equation iteratively using Newton's method.
    Direct port of Vyper contract logic.

    Args:
        i: Index of the input coin.
        j: Index of the output coin.
        x: The new value of x[i].
        xp: Token balances adjusted to equal precision.
        amp: The amplification coefficient A (already resolved for ramping
            if needed, WITH or WITHOUT A_PRECISION divisor depending on y_variant).
        n_coins: Number of coins in the pool.
        a_precision: Precision factor for A (typically 100).
        y_variant: Which Y-step variant to use.
        d_variant: Which D-step variant to use (for internal D computation).

    Returns:
        The computed y value (x[j]).

    Raises:
        ValueError: If y calculation does not converge within 255 iterations.
        AssertionError: If i == j or indices are out of range.
    """
    assert i != j, "same coin"
    assert j >= 0, "j below zero"
    assert j < n_coins, "j above N_COINS"
    assert i >= 0
    assert i < n_coins

    c: int = stableswap_get_d(xp, amp, n_coins, a_precision, d_variant)
    y: int = c
    d: int = c

    s = 0
    for coin_index in range(n_coins):
        if coin_index == i:
            x_ = x
        elif coin_index != j:
            x_ = xp[coin_index]
        else:
            continue
        s += x_
        c = c * d // (x_ * n_coins)

    a_nn = amp * n_coins
    if y_variant in {y_variant.VARIANT_0, y_variant.VARIANT_1}:
        c = c * d // (a_nn * n_coins)
        b = s + d // a_nn
    else:
        c = c * d * a_precision // (a_nn * n_coins)
        b = s + d * a_precision // a_nn

    for _ in range(255):  # pragma: no branch
        y_prev = y
        y = (y * y + c) // (2 * y + b - d)
        if y > y_prev:
            if y - y_prev <= 1:
                return y
        elif y_prev - y <= 1:
            return y

    msg = "y calculation did not converge"
    raise ValueError(msg)


def stableswap_get_y_d(
    a: int,
    i: int,
    *,
    xp: Sequence[int],
    d: int,
    n_coins: int,
    a_precision: int,
    yd_variant: YDVariant,
) -> int:
    """Calculate y given A, xp, and D in a Curve StableSwap pool.

    Used by calc_token_amount and calc_withdraw_one_coin.
    Direct port of Vyper contract logic.

    Args:
        a: The amplification coefficient A (pre-divided by A_PRECISION if needed).
        i: Index of the coin to solve for.
        xp: Token balances adjusted to equal precision.
        d: The invariant D value.
        n_coins: Number of coins in the pool.
        a_precision: Precision factor for A (typically 100).
        yd_variant: Which Y_D-step variant to use.

    Returns:
        The computed y value.

    Raises:
        ValueError: If y calculation does not converge within 255 iterations.
        AssertionError: If indices are out of range.
    """
    assert i >= 0  # dev: i below zero
    assert i < n_coins  # dev: i above N_COINS

    c: int = d
    y: int = d

    s = 0
    for coin_index in range(n_coins):
        if coin_index != i:
            x = xp[coin_index]
        else:
            continue
        s += x
        c = c * d // (x * n_coins)

    a_nn = a * n_coins
    if yd_variant == yd_variant.VARIANT_0:
        b = s + d * a_precision // a_nn
        c = c * d * a_precision // (a_nn * n_coins)
    else:
        b = s + d // a_nn
        c = c * d // (a_nn * n_coins)

    for _ in range(255):  # pragma: no branch
        y_prev = y
        y = (y * y + c) // (2 * y + b - d)
        if y > y_prev:
            if y - y_prev <= 1:
                return y
        elif y_prev - y <= 1:
            return y

    msg = "y_d calculation did not converge"
    raise ValueError(msg)


def stableswap_newton_y(
    ann: int,
    gamma: int,
    *,
    xp: Sequence[int],
    d: int,
    token_index: int,
    n_coins: int,
    a_multiplier: int,
) -> int:
    """Calculate xp[i] given other balances and invariant D, using Newton's method.

    Used by crypto (volatile) Curve pools.
    Direct port of Vyper contract logic.

    Args:
        ann: A * N^N.
        gamma: Pool gamma parameter.
        xp: Token balances adjusted to equal precision.
        d: The invariant D value.
        token_index: Index of the coin to solve for.
        n_coins: Number of coins in the pool.
        a_multiplier: A precision multiplier (typically A_PRECISION = 100).

    Returns:
        The computed y value.

    Raises:
        EVMRevertError: If Newton's method does not converge.
        AssertionError: If safety checks fail (unsafe A, gamma, D values).
    """
    # Safety checks
    assert (
        n_coins**n_coins * a_multiplier - 1 < ann < 10000 * n_coins**n_coins * a_multiplier + 1
    ), "unsafe value for A"
    assert 10**10 - 1 < gamma < 10**16 + 1, "unsafe values for gamma"
    assert 10**17 - 1 < d < 10**15 * 10**18 + 1, "unsafe values for D"

    for index in range(n_coins):
        if index != token_index:
            frac = xp[index] * 10**18 // d
            assert 10**16 - 1 < frac < 10**20 + 1, (  # dev: unsafe values x[i]
                f"{frac=} out of range"
            )

    y = d // n_coins
    k_0_i = 10**18
    s_i = 0

    x_sorted = list(xp)
    x_sorted[token_index] = 0
    x_sorted = sorted(x_sorted, reverse=True)  # From high to low

    convergence_limit = max(x_sorted[0] // 10**14, d // 10**14, 100)
    for j_ in range(2, n_coins + 1):
        x_ = x_sorted[n_coins - j_]
        y = y * d // (x_ * n_coins)  # Small _x first
        s_i += x_

    for k_ in range(n_coins - 1):
        k_0_i = k_0_i * x_sorted[k_] * n_coins // d  # Large _x first

    for _ in range(255):  # pragma: no branch
        y_prev = y

        k_0 = k_0_i * y * n_coins // d
        s = s_i + y

        g1k0 = gamma + 10**18
        g1k0 = g1k0 - k_0 + 1 if g1k0 > k_0 else k_0 - g1k0 + 1

        mul1 = 10**18 * d // gamma * g1k0 // gamma * g1k0 * a_multiplier // ann
        mul2 = 10**18 + (2 * 10**18) * k_0 // g1k0

        yfprime = 10**18 * y + s * mul2 + mul1
        dyfprime = d * mul2

        if yfprime < dyfprime:
            y = y_prev // 2
            continue

        yfprime -= dyfprime
        fprime = yfprime // y

        y_minus = mul1 // fprime
        y_plus = (yfprime + 10**18 * d) // fprime + y_minus * 10**18 // k_0
        y_minus += 10**18 * s // fprime

        y = y_prev // 2 if y_plus < y_minus else y_plus - y_minus
        diff = y - y_prev if y > y_prev else y_prev - y

        if diff < max(convergence_limit, y // 10**14):
            frac = y * 10**18 // d
            assert 10**16 - 1 < frac < 10**20 + 1, "unsafe value for y"
            return y

    msg = "_newton_y() did not converge"
    raise ValueError(msg)


def stableswap_reduction_coefficient(
    x: Sequence[int],
    fee_gamma: int,
    n_coins: int,
) -> int:
    """fee_gamma / (fee_gamma + (1 - K)) where K = prod(x) / (sum(x) / N)**N.

    All values normalized to 1e18.
    Used by crypto (volatile) Curve pools for dynamic fee calculation.
    """
    k = 10**18
    s = 0
    for x_i in x:
        s += x_i
    for x_i in x:
        k = k * n_coins * x_i // s
    if fee_gamma > 0:
        k = fee_gamma * 10**18 // (fee_gamma + 10**18 - k)
    return k
