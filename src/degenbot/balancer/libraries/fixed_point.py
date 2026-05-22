"""Balancer V2 FixedPoint arithmetic helpers."""
from degenbot.balancer.libraries import log_exp_math
from degenbot.balancer.libraries.constants import FOUR, MAX_POW_RELATIVE_ERROR, ONE, TWO, PowVersion
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.pool import EVMRevertError

_ZERO = 0


def add(a: int, b: int) -> int:
    """Return add."""
    if a + b > MAX_UINT256:
        raise EVMRevertError(error="ADD_OVERFLOW")
    return a + b


def sub(a: int, b: int) -> int:
    """Return sub."""
    if b > a:
        raise EVMRevertError(error="SUB_OVERFLOW")
    return a - b


def mul_down(a: int, b: int) -> int:
    """Return mul down."""
    product = a * b
    if not (a == 0 or product // a == b):
        raise EVMRevertError(error="MUL_OVERFLOW")
    return product // ONE


def mul_up(a: int, b: int) -> int:
    """Return mul up."""
    product = a * b

    if product == 0:
        return _ZERO
    if product > MAX_UINT256:
        raise EVMRevertError(error="MUL_OVERFLOW")

    return (product - 1) // ONE + 1


def div_down(a: int, b: int) -> int:
    """Return div down."""
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return a_inflated // b


def div_up(a: int, b: int) -> int:
    """Return div up."""
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return ((a_inflated - 1) // b) + 1


def pow_down(x: int, y: int, *, version: PowVersion = PowVersion.V1) -> int:
    """Return x^y, assuming both are fixed point numbers, rounding down. The result is guaranteed to.

    not be above the true value (that is, the error function expected - actual is always positive).

    The `version` parameter controls which deployed contract implementation to match:
    - V1: General path only (no fast paths). Used by WeightedPool2Tokens and other older contracts.
    - V2: Includes fast paths for y == ONE, TWO, FOUR. Used by newer WeightedPool contracts.
    """
    if version == PowVersion.V2:
        if y == ONE:
            return x
        if y == TWO:
            return mul_down(x, x)
        if y == FOUR:
            square = mul_down(x, x)
            return mul_down(square, square)

    raw = log_exp_math.pow(x, y)
    max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR), 1)
    if raw < max_error:
        return 0

    return sub(raw, max_error)


def pow_up(x: int, y: int, *, version: PowVersion = PowVersion.V1) -> int:
    """Return x^y, assuming both are fixed point numbers, rounding up. The result is guaranteed to not.

    be below the true value (that is, the error function expected - actual is always negative).

    The `version` parameter controls which deployed contract implementation to match:
    - V1: General path only (no fast paths). Used by WeightedPool2Tokens and other older contracts.
    - V2: Includes fast paths for y == ONE, TWO, FOUR. Used by newer WeightedPool contracts.
    """
    if version == PowVersion.V2:
        if y == ONE:
            return x
        if y == TWO:
            return mul_up(x, x)
        if y == FOUR:
            square = mul_up(x, x)
            return mul_up(square, square)

    raw = log_exp_math.pow(x, y)
    max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR), 1)
    return add(raw, max_error)


def complement(x: int) -> int:
    """Return complement."""
    return ONE - x if x < ONE else _ZERO
