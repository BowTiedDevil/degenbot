"""Solidly StableSwap invariant calculations.

Pure functions implementing the Solidly stable pool invariant
(x^3*y + y^3*x >= k) and its Newton's method solvers.

These functions are shared across Solidly-derived DEXes (Aerodrome, Camelot, Velodrome).
DEX-specific wrappers that capture fee/decimals via closures remain in their DEX modules.
"""

from collections.abc import Callable
from fractions import Fraction

from degenbot.calculations.evm_math import raise_if_invalid_uint256
from degenbot.exceptions.pool import EVMRevertError


def calc_d(x0: int, y: int) -> int:
    """Calculate the Solidly stable D value for normalized reserves x0, y.

    D = 3*x0*y^2 + x0^3*y, all scaled by 10^18.
    """
    return (3 * x0 * ((y * y) // 10**18)) // 10**18 + ((((x0 * x0) // 10**18) * x0) // 10**18)


def calc_k(
    balance_0: int,
    balance_1: int,
    decimals_0: int,
    decimals_1: int,
) -> int:
    """Calculate the Solidly stable k invariant for unnormalized reserves.

    Scales reserves by decimals, then computes k = a*b where a = x*y, b = x^2 + y^2.
    """
    x = balance_0 * 10**18 // decimals_0
    y = balance_1 * 10**18 // decimals_1
    a = (x * y) // 10**18
    b = (x * x) // 10**18 + (y * y) // 10**18
    raise_if_invalid_uint256(a * b)
    return a * b // 10**18


def calc_f(x0: int, y: int) -> int:
    """Evaluate the Solidly stable invariant f(x0, y) = x0*y^3 + x0^3*y.

    Used by Newton's method solvers to find y given x0 and target k.
    """
    a = (x0 * y) // 10**18
    b = (x0 * x0) // 10**18 + (y * y) // 10**18
    return (a * b) // 10**18


def calc_exact_in_stable(
    *,
    amount_in: int,
    token_in: int,
    reserves0: int,
    reserves1: int,
    decimals0: int,
    decimals1: int,
    fee: Fraction,
    k_func: Callable[[int, int, int, int], int],
    get_y_func: Callable[[int, int, int, int, int], int],
) -> int:
    """Calculate the amount out for an exact input to a Solidly stable pool.

    Generic over the k() and get_y() implementations to accommodate
    DEX-specific variants (Aerodrome, Camelot).
    """
    from degenbot.exceptions.base import DegenbotValueError

    if token_in not in {0, 1}:  # pragma: no cover
        msg = "Invalid token_in identifier"
        raise DegenbotValueError(message=msg)

    try:
        amount_in_after_fee = amount_in - amount_in * fee.numerator // fee.denominator

        xy = k_func(reserves0, reserves1, decimals0, decimals1)

        scaled_reserves_0 = (reserves0 * 10**18) // decimals0
        scaled_reserves_1 = (reserves1 * 10**18) // decimals1

        if token_in == 0:
            reserves_a, reserves_b = scaled_reserves_0, scaled_reserves_1
            amount_in_after_fee = (amount_in_after_fee * 10**18) // decimals0
        else:
            reserves_a, reserves_b = scaled_reserves_1, scaled_reserves_0
            amount_in_after_fee = (amount_in_after_fee * 10**18) // decimals1

        y = reserves_b - get_y_func(
            amount_in_after_fee + reserves_a, xy, reserves_b, decimals0, decimals1
        )
        return (y * (decimals1 if token_in == 0 else decimals0)) // 10**18
    except ZeroDivisionError:
        msg = "Division by zero"
        raise EVMRevertError(error=msg) from None


def calc_exact_in_volatile(
    amount_in: int,
    token_in: int,
    reserves0: int,
    reserves1: int,
    fee: Fraction,
) -> int:
    """Calculate the amount out for an exact input to a Solidly volatile pool (x*y>=k)."""
    from degenbot.exceptions.base import DegenbotValueError

    if token_in not in {0, 1}:  # pragma: no cover
        msg = "Invalid token_in identifier"
        raise DegenbotValueError(message=msg)

    amount_in_after_fee = amount_in - amount_in * fee.numerator // fee.denominator
    reserves_a, reserves_b = (reserves0, reserves1) if token_in == 0 else (reserves1, reserves0)
    return (amount_in_after_fee * reserves_b) // (reserves_a + amount_in_after_fee)


def get_y_solidly(
    x0: int,
    xy: int,
    y: int,
    decimals0: int,
    decimals1: int,
) -> int:
    """Solve for y in the Solidly stable invariant using Newton's method.

    Finds the minimum y such that f(x0, y) >= xy.

    Reference: https://github.com/aerodrome-finance/contracts/blob/main/contracts/Pool.sol
    """
    for _ in range(255):
        k = calc_f(x0, y)
        if k < xy:
            dy = ((xy - k) * 10**18) // calc_d(x0, y)
            if dy == 0:
                if k == xy:
                    return y
                if (
                    calc_k(
                        balance_0=x0,
                        balance_1=y + 1,
                        decimals_0=decimals0,
                        decimals_1=decimals1,
                    )
                    > xy
                ):
                    return y + 1
                dy = 1
            y += dy
        else:
            dy = ((k - xy) * 10**18) // calc_d(x0, y)
            if dy == 0:
                if k == xy or calc_f(x0, y - 1) < xy:
                    return y
                dy = 1
            y -= dy
    raise EVMRevertError(error="Failed to converge on a value for y")
