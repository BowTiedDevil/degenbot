"""Uniswap V3 UnsafeMath: rounding-up division.

Rust-accelerated implementation used by default.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol (UnsafeMath library)
"""

from degenbot.degenbot_rs import cl_div_rounding_up as div_rounding_up
