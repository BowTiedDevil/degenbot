"""Uniswap V4 LiquidityMath: liquidity delta from tick crosses.

See: contract_reference/uniswap/V4/PoolManager.sol (LiquidityMath library)
"""
from degenbot.constants import MAX_UINT128, MIN_UINT128
from degenbot.exceptions.pool import EVMRevertError


def add_delta(
    x: int,
    y: int,
) -> int:
    """Check that the result fits in a uint128.

    Instead of inline Yul as implemented by the Solidity contract.

    Returns:
        The sum x + y, guaranteed to fit in a uint128.

    Raises:
        EVMRevertError: If the result overflows or underflows uint128 bounds.

    ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/LiquidityMath.sol

    """
    result = x + y
    if result < MIN_UINT128 or result > MAX_UINT128:
        msg = "SafeCastOverflow"
        raise EVMRevertError(msg)

    return result
