"""Uniswap V4 FullMath: 512-bit multiplication with Q96 rounding.

Rust-accelerated implementation used by default.

See: contract_reference/uniswap/V4/PoolManager.sol (FullMath library)
"""

import functools
from collections.abc import Callable
from typing import Any

from degenbot.degenbot_rs import (
    cl_muldiv as _rs_muldiv,
)
from degenbot.degenbot_rs import (
    cl_muldiv_rounding_up as _rs_muldiv_rounding_up,
)
from degenbot.exceptions.pool import EVMRevertError

# Translation table: Rust core messages → V4 Solidity revert messages
_V4_MESSAGE_MAP = {
    "DIVISION BY ZERO": "required: denominator > 0",
}


def _wrap(fn: Callable[..., Any]) -> Callable[..., Any]:
    """Wrap Rust function to convert ValueError/OverflowError → EVMRevertError.

    Returns:
        A wrapper function that re-raises as EVMRevertError.

    """

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:  # noqa: ANN401
        try:
            return fn(*args, **kwargs)
        except (ValueError, OverflowError) as e:
            msg = _V4_MESSAGE_MAP.get(str(e), str(e))
            raise EVMRevertError(error=msg) from e

    return wrapper


muldiv = _wrap(_rs_muldiv)
muldiv_rounding_up = _wrap(_rs_muldiv_rounding_up)
