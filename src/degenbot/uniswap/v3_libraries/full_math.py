"""Uniswap V3 FullMath: 512-bit multiplication with Q96 rounding.

Thin delegation shim over the Rust ``degenbot-cl-math`` core
(``cl_muldiv`` / ``cl_muldiv_rounding_up``), exposing them with the V3
Solidity revert messages via ``EVMRevertError``.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (FullMath library)
"""

import functools
from collections.abc import Callable
from typing import Any

from degenbot.degenbot_rs import cl_muldiv as _rs_muldiv
from degenbot.degenbot_rs import cl_muldiv_rounding_up as _rs_muldiv_rounding_up
from degenbot.exceptions.pool import EVMRevertError

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
