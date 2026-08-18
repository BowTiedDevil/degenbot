"""
Port of UniswapV3 FixedPoint96.sol.

Source: contracts/libraries/FixedPoint96.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/FixedPoint96.sol

Defines the Q64.96 fixed-point format used throughout V3 for sqrt prices.
"""

RESOLUTION: constant(uint8) = 96
Q96: constant(uint256) = 79228162514264337593543950336  # 2^96 = 0x1000000000000000000000000
