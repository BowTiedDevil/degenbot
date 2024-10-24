from collections.abc import Callable
from fractions import Fraction
from typing import Literal

from degenbot.exceptions import DegenbotValueError, EVMRevertError
from degenbot.functions import raise_if_invalid_uint256


def general_calc_d(
    x0: int,
    y: int,
) -> int:
    return (3 * x0 * ((y * y) // 10**18)) // 10**18 + ((((x0 * x0) // 10**18) * x0) // 10**18)


def general_calc_k(
    balance_0: int,
    balance_1: int,
    decimals_0: int,
    decimals_1: int,
) -> int:
    _x = balance_0 * 10**18 // decimals_0
    _y = balance_1 * 10**18 // decimals_1
    _a = (_x * _y) // 10**18
    _b = (_x * _x) // 10**18 + (_y * _y) // 10**18
    raise_if_invalid_uint256(_a * _b)
    return _a * _b // 10**18  # x^3*y + y^3*x >= k


def general_calc_exact_in_stable(
    amount_in: int,
    token_in: Literal[0, 1],
    reserves0: int,
    reserves1: int,
    decimals0: int,
    decimals1: int,
    fee: Fraction,
    k_func: Callable[
        [int, int, int, int],
        int,
    ],
    get_y_func: Callable[
        [int, int, int, int, int],
        int,
    ],
) -> int:
    """
    Calculate the amount out for an exact input to a Solidly stable pool (invariant
    y*x^3 + x*y^3 >= k).

    This function is generic and requires a callable k() and get_y()
    """

    if token_in not in (0, 1):  # pragma: no cover
        raise DegenbotValueError(message="Invalid token_in identifier")

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
        # Pools with very low reserves can throw division by zero errors because _d() returns 0
        raise EVMRevertError(error="Division by zero") from None


def general_calc_exact_in_volatile(
    amount_in: int,
    token_in: Literal[0, 1],
    reserves0: int,
    reserves1: int,
    fee: Fraction,
) -> int:
    """
    Calculate the amount out for an exact input to a Solidly volatile pool (invariant x*y>=k).
    """

    if token_in not in (0, 1):  # pragma: no cover
        raise DegenbotValueError(message="Invalid token_in identifier")

    amount_in_after_fee = amount_in - amount_in * fee.numerator // fee.denominator
    reserves_a, reserves_b = (reserves0, reserves1) if token_in == 0 else (reserves1, reserves0)
    return (amount_in_after_fee * reserves_b) // (reserves_a + amount_in_after_fee)
