from degenbot.balancer.libraries import log_exp_math
from degenbot.balancer.libraries.constants import MAX_POW_RELATIVE_ERROR, ONE
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.pool import EVMRevertError

_ZERO = 0


def add(a: int, b: int) -> int:
    if a + b > MAX_UINT256:
        raise EVMRevertError(error="ADD_OVERFLOW")
    return a + b


def sub(a: int, b: int) -> int:
    if b > a:
        raise EVMRevertError(error="SUB_OVERFLOW")
    return a - b


def mul_down(a: int, b: int) -> int:
    product = a * b
    if not (a == 0 or product // a == b):
        raise EVMRevertError(error="MUL_OVERFLOW")
    return product // ONE


def mul_up(a: int, b: int) -> int:
    product = a * b

    if product == 0:
        return _ZERO
    if product > MAX_UINT256:
        raise EVMRevertError(error="MUL_OVERFLOW")

    return (product - 1) // ONE + 1


def div_down(a: int, b: int) -> int:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return a_inflated // b


def div_up(a: int, b: int) -> int:
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return ((a_inflated - 1) // b) + 1


def pow_down(x: int, y: int) -> int:
    """
    Returns x^y, assuming both are fixed point numbers, rounding down. The result is guaranteed to
    not be above the true value (that is, the error function expected - actual is always positive).
    """

    # Note: the on-chain contract does NOT have fast paths for y == 1, y == 2, or y == 4 — those
    # were added to a later version of the monorepo. The deployed WeightedPool2Tokens uses the
    # general path via LogExpMath.pow for all cases, so we must match that behavior exactly.
    # In particular, the general path applies an error bound (max_error) that the fast paths skip,
    # causing different rounding behavior that cascades through complement() and mul operations.

    raw = log_exp_math.pow(x, y)
    max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR), 1)
    if raw < max_error:
        return 0

    return sub(raw, max_error)


def pow_up(x: int, y: int) -> int:
    """
    Returns x^y, assuming both are fixed point numbers, rounding up. The result is guaranteed to not
    be below the true value (that is, the error function expected - actual is always negative).
    """

    # Note: the on-chain contract does NOT have fast paths for y == 1, y == 2, or y == 4 — those
    # were added to a later version of the monorepo. The deployed WeightedPool2Tokens uses the
    # general path via LogExpMath.pow for all cases, so we must match that behavior exactly.
    # In particular, the general path applies an error bound (max_error) that the fast paths skip,
    # causing different rounding behavior that cascades through complement() and mul operations.

    raw = log_exp_math.pow(x, y)
    max_error = add(mul_up(raw, MAX_POW_RELATIVE_ERROR), 1)
    return add(raw, max_error)


def complement(x: int) -> int:
    return ONE - x if x < ONE else _ZERO
