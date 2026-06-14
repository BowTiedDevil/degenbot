"""Uniswap V4 FullMath: 512-bit multiplication with Q96 rounding.

Rust-accelerated implementation used by default.

See: contract_reference/uniswap/V4/PoolManager.sol (FullMath library)
"""

from degenbot.degenbot_rs import (
    cl_muldiv as _rs_muldiv,
    cl_muldiv_rounding_up as _rs_muldiv_rounding_up,
)
from degenbot.exceptions.pool import EVMRevertError

from degenbot.uniswap.v4_libraries.functions import mulmod

# Translation table: Rust core messages → V4 Solidity revert messages
_V4_MESSAGE_MAP = {
    "DIVISION BY ZERO": "required: denominator > 0",
}


def _wrap(fn):
    def wrapper(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except (ValueError, OverflowError) as e:
            msg = _V4_MESSAGE_MAP.get(str(e), str(e))
            raise EVMRevertError(error=msg) from e
        except OverflowError as e:
            raise EVMRevertError(error=str(e)) from e
    wrapper.__name__ = fn.__name__
    return wrapper


muldiv = _wrap(_rs_muldiv)
muldiv_rounding_up = _wrap(_rs_muldiv_rounding_up)
