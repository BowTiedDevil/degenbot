"""Uniswap V3 BitMath: bit position and population count.

Rust-accelerated implementation used by default, with input validation
matching the V3 Solidity contract's revert behavior.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (BitMath library)
"""

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.degenbot_rs import (
    cl_least_significant_bit as _rs_least_significant_bit,
)
from degenbot.degenbot_rs import (
    cl_most_significant_bit as _rs_most_significant_bit,
)
from degenbot.exceptions.pool import EVMRevertError


def least_significant_bit(number: int) -> int:
    """Find the least significant bit for the given number.

    Returns:
        The index of the least significant bit.

    Raises:
        EVMRevertError: If number is out of uint256 range.

    """
    if number <= MIN_UINT256:
        raise EVMRevertError(error="required: number > 0")
    if number > MAX_UINT256:
        raise EVMRevertError(error="required: number <= max(uint256)")
    return _rs_least_significant_bit(number)


def most_significant_bit(number: int) -> int:
    """Find the most significant bit for the given number.

    Returns:
        The index of the most significant bit.

    Raises:
        EVMRevertError: If number is out of uint256 range.

    """
    if number <= MIN_UINT256:
        raise EVMRevertError(error="required: number >= 0 ")
    if number > MAX_UINT256:
        raise EVMRevertError(error="required: number <= max(uint256) ")
    return _rs_most_significant_bit(number)
