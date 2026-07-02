"""Balancer V2 FixedPoint arithmetic helpers.

Trimmed to the three live fns consumed by ``scaling_helpers.py``:
``div_down``, ``div_up``, ``mul_down``. The dead leaves (``add``,
``sub``, ``complement``, ``mul_up``, ``pow_down``, ``pow_up``) were
retired alongside ``log_exp_math.py``: their Rust counterparts live in
``degenbot-balancer-math`` (``fixed_point.rs`` / ``log_exp_math.rs``)
with their own ``#[cfg(test)]`` corpora, and the Python parity oracle
had zero src consumers after the ``stable_math.py`` retirement. The
remaining three fns retire under Candidate 4 (route ``scaling_helpers``
through the Rust leaf once it is exposed via ``#[pyfunction]``).
"""

from degenbot.balancer.libraries.constants import ONE
from degenbot.constants import MAX_UINT256
from degenbot.exceptions.pool import EVMRevertError

_ZERO = 0


def mul_down(a: int, b: int) -> int:
    """Return ``a * b / ONE``, rounding down.

    Returns:
        The computed integer value.

    Raises:
        EVMRevertError: See function documentation.

    """
    product = a * b
    if not (a == 0 or product // a == b):
        raise EVMRevertError(error="MUL_OVERFLOW")
    return product // ONE


def div_down(a: int, b: int) -> int:
    """Return ``a * ONE / b``, rounding down.

    Returns:
        The computed integer value.

    Raises:
        EVMRevertError: See function documentation.

    """
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return a_inflated // b


def div_up(a: int, b: int) -> int:
    """Return ``a * ONE / b``, rounding up.

    Returns:
        The computed integer value.

    Raises:
        EVMRevertError: See function documentation.

    """
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")

    if a == 0:
        return _ZERO

    a_inflated = a * ONE
    if a_inflated > MAX_UINT256:
        raise EVMRevertError(error="DIV_INTERNAL")

    return ((a_inflated - 1) // b) + 1
