from degenbot.balancer.libraries import log_exp_math
from degenbot.balancer.libraries.constants import FOUR, MAX_POW_RELATIVE_ERROR, ONE, TWO
from degenbot.constants import MAX_UINT256
from degenbot.exceptions import EVMRevertError


def add(a: int, b: int) -> int:
    assert isinstance(a, int)
    assert isinstance(b, int)

    if a + b > MAX_UINT256:
        raise EVMRevertError(error="ADD_OVERFLOW")
    return a + b


def sub(a: int, b: int) -> int:
    assert isinstance(a, int)
    assert isinstance(b, int)

    if b > a:
        raise EVMRevertError(error="SUB_OVERFLOW")
    return a - b


def mulDown(a: int, b: int):
    assert isinstance(a, int)
    assert isinstance(b, int)

    product = a * b
    if not (a == 0 or product // a == b):
        raise EVMRevertError(error="MUL_OVERFLOW")
    return product // ONE


def mulUp(a: int, b: int):
    assert isinstance(a, int)
    assert isinstance(b, int)

    product = a * b
    if not (a == 0 or product // a == b):
        raise EVMRevertError(error="MUL_OVERFLOW")
    return 0 if product == 0 else (product - 1) // ONE + 1


def divDown(a: int, b: int) -> int:
    assert isinstance(a, int)
    assert isinstance(b, int)
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    a_inflated = a * ONE
    if not (a == 0 or a_inflated // a == ONE):  # TODO: rework since Python ints won't overflow
        raise EVMRevertError(error="DIV_INTERNAL")

    return a_inflated // b


def divUp(a: int, b: int) -> int:
    assert isinstance(a, int)
    assert isinstance(b, int)

    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    a_inflated = a * ONE
    if not (a == 0 or a_inflated / a == ONE):
        raise EVMRevertError(error="DIV_INTERNAL")

    return 0 if a == 0 else ((a * ONE - 1) // b) + 1


def powDown(x: int, y: int):
    """
    Returns x^y, assuming both are fixed point numbers, rounding down. The result is guaranteed to
    not be above the true value (that is, the error function expected - actual is always positive).
    """

    assert isinstance(x, int)
    assert isinstance(y, int)

    # Optimize for when y equals 1.0, 2.0 or 4.0, as those are very simple to implement and occur often in 50/50
    # and 80/20 Weighted Pools
    if y == ONE:
        return x
    if y == TWO:
        return mulDown(x, x)
    if y == FOUR:
        square = mulDown(x, x)
        return mulDown(square, square)

    raw = log_exp_math.pow(x, y)
    max_error = add(mulUp(raw, MAX_POW_RELATIVE_ERROR), 1)
    if raw < max_error:
        return 0

    return sub(raw, max_error)


def powUp(x: int, y: int) -> int:
    """
    Returns x^y, assuming both are fixed point numbers, rounding up. The result is guaranteed to not
    be below the true value (that is, the error function expected - actual is always negative).
    """

    assert isinstance(x, int)
    assert isinstance(y, int)

    # Optimize for when y equals 1.0, 2.0 or 4.0, as those are very simple to implement and occur
    # often in 50/50 and 80/20 Weighted Pools
    if y == ONE:
        return x
    if y == TWO:
        return mulUp(x, x)
    if y == FOUR:
        square = mulUp(x, x)
        return mulUp(square, square)

    raw = log_exp_math.pow(x, y)
    max_error = add(mulUp(raw, MAX_POW_RELATIVE_ERROR), 1)
    return add(raw, max_error)


def complement(x: int) -> int:
    return ONE - x if x < ONE else 0
