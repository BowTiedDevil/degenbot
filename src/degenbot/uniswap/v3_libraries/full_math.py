"""Uniswap V3 FullMath: 512-bit multiplication with Q96 rounding.

Rust-accelerated implementation used by default.
Python implementation preserved as ``_py_muldiv`` / ``_py_muldiv_rounding_up`` for testing.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (FullMath library)
"""

import functools
from collections.abc import Callable
from typing import Any

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.degenbot_rs import cl_muldiv as _rs_muldiv
from degenbot.degenbot_rs import cl_muldiv_rounding_up as _rs_muldiv_rounding_up
from degenbot.exceptions.pool import EVMRevertError
from degenbot.uniswap.v3_libraries.functions import mulmod

# Translation table: Rust core messages → V3 Solidity revert messages
_V3_MESSAGE_MAP = {
    "DIVISION BY ZERO": "DIVISION BY ZERO",
}


def _wrap_evmrevert(fn: Callable[..., Any]) -> Callable[..., Any]:
    """Wrap Rust function to convert ValueError/OverflowError → EVMRevertError.

    Returns:
        A wrapper function that re-raises as EVMRevertError.

    """

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:  # noqa: ANN401
        try:
            return fn(*args, **kwargs)
        except ValueError as e:
            msg = _V3_MESSAGE_MAP.get(str(e), str(e))
            raise EVMRevertError(error=msg) from e
        except OverflowError as e:
            raise EVMRevertError(error=str(e)) from e

    return wrapper


muldiv = _wrap_evmrevert(_rs_muldiv)
muldiv_rounding_up = _wrap_evmrevert(_rs_muldiv_rounding_up)


def _py_muldiv(
    a: int,
    b: int,
    denominator: int,
) -> int:
    """Compute a * b / d with full 512-bit precision (Python fallback).

    Returns:
        The result of (a * b) // denominator.

    Raises:
        EVMRevertError: If inputs are out of uint256 range, denominator is zero,
            or result overflows.

    """
    if a < MIN_UINT256 or a > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for a.")
    if b < MIN_UINT256 or b > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for b.")
    if denominator < MIN_UINT256 or denominator > MAX_UINT256:
        raise EVMRevertError(error="Invalid value for denominator.")

    if denominator == 0:
        raise EVMRevertError(error="DIVISION BY ZERO")

    result = (a * b) // denominator

    if not (MIN_UINT256 <= result <= MAX_UINT256):
        raise EVMRevertError(error="Invalid result, does not fit in uint256")

    return result


def _py_muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    """Return muldiv rounding up (Python fallback).

    Returns:
        The result of (a * b + d - 1) // denominator (rounded up).

    Raises:
        EVMRevertError: If the result would overflow uint256.

    """
    result = _py_muldiv(a, b, denominator)
    if mulmod(a, b, denominator) > 0:
        if not (MIN_UINT256 <= result < MAX_UINT256):
            raise EVMRevertError(error="FAIL!")
        return result + 1
    return result
