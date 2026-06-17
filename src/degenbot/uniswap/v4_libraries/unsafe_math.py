"""Uniswap V4 UnsafeMath: rounding-up division and simple mul-div.

Rust-accelerated implementation used by default.

See: contract_reference/uniswap/V4/PoolManager.sol (UnsafeMath library)
"""

from degenbot.degenbot_rs import (
    cl_div_rounding_up as div_rounding_up,  # noqa: F401
)
from degenbot.degenbot_rs import (
    cl_simple_mul_div as simple_mul_div,  # noqa: F401
)
