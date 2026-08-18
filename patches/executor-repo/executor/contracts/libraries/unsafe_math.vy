"""
Port of UniswapV3 UnsafeMath.sol.

Source: contracts/libraries/UnsafeMath.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/UnsafeMath.sol
"""


@internal
@pure
def div_rounding_up(x: uint256, y: uint256) -> uint256:
    """Returns ceil(x / y), i.e., rounds up the division.

    Equivalent to Solidity:
        assembly { z := add(div(x, y), gt(mod(x, y), 0)) }

    Dev note: division by 0 has unspecified behavior (matches Solidity).
    """
    return x // y + convert(x % y > 0, uint256)
