"""Uniswap V3 LiquidityMath: liquidity delta from tick crosses.

Rust-accelerated implementation used by default.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (LiquidityMath library)
"""

from degenbot.constants import MAX_INT128, MAX_UINT128, MIN_INT128
from degenbot.degenbot_rs import cl_add_delta as _rs_add_delta
from degenbot.exceptions.pool import EVMRevertError


def add_delta(x: int, y: int) -> int:
    """Add a signed delta to a uint128 liquidity value.

    Delegates to Rust implementation.  Input validation runs in Python
    so that V3-specific revert messages match the Solidity contract.
    """
    # Validate x fits in uint128
    if x < 0 or x > MAX_UINT128:
        raise EVMRevertError(error="LA")
    # Validate y fits in int128
    if y < MIN_INT128 or y > MAX_INT128:
        raise EVMRevertError(error="LA")
    try:
        return _rs_add_delta(x, y)
    except ValueError as e:
        # Rust returns SafeCastOverflow for both add and subtract overflow.
        # V3 Solidity distinguishes: LA (add overflow) vs LS (subtract underflow)
        if y >= 0:
            raise EVMRevertError(error="LA") from e
        else:
            raise EVMRevertError(error="LS") from e
    except OverflowError as e:
        if y >= 0:
            raise EVMRevertError(error="LA") from e
        else:
            raise EVMRevertError(error="LS") from e
