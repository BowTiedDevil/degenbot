"""
Camelot invariant calculations.

Pure functions implementing the Camelot DEX's variant of the Solidly stable invariant.
Camelot uses its own f() and k() implementations that differ slightly from the
Solidly/Aerodrome versions.

The get_y solver is also Camelot-specific (different convergence check).
"""

from degenbot.calculations.evm_math import raise_if_invalid_uint256
from degenbot.calculations.solidly_stable import calc_d as solidly_calc_d


def f_camelot(x0: int, y: int) -> int:
    """
    Evaluate the Camelot invariant f(x0, y) = x0*y^3 + x0^3*y.

    Same mathematical form as Solidly/Aerodrome but with different operation ordering
    to match the Camelot contract's arithmetic.
    """
    return (
        x0 * (y * y // 10**18 * y // 10**18) // 10**18
        + (x0 * x0 // 10**18 * x0 // 10**18) * y // 10**18
    )


def k_camelot(
    balance_0: int,
    balance_1: int,
    decimals_0: int,
    decimals_1: int,
) -> int:
    """
    Calculate the Camelot k invariant for unnormalized reserves.

    Same mathematical form as Solidly calc_k but exposed as a separate function
    because Camelot codebases import it by name.
    """
    x = balance_0 * 10**18 // decimals_0
    y = balance_1 * 10**18 // decimals_1
    a = (x * y) // 10**18
    b = (x * x) // 10**18 + (y * y) // 10**18
    raise_if_invalid_uint256(a * b)
    return a * b // 10**18


def get_y_camelot(
    x_0: int,
    xy: int,
    y: int,
) -> int:
    """
    Solve for y in the Camelot invariant using Newton's method.

    Uses the Solidly calc_d (same D function) but the Camelot f() and a
    simpler convergence check (delta <= 1 in either direction).
    """
    for _ in range(255):
        y_prev = y
        k = f_camelot(x_0, y)
        if k < xy:
            dy = (xy - k) * 10**18 // solidly_calc_d(x_0, y)
            y += dy
        else:
            dy = (k - xy) * 10**18 // solidly_calc_d(x_0, y)
            y -= dy

        if y > y_prev:
            if y - y_prev <= 1:
                return y
        elif y_prev - y <= 1:
            return y
    return y
