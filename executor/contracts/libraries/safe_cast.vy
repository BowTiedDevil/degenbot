"""
Port of UniswapV3 SafeCast.sol.

Source: contracts/libraries/SafeCast.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/SafeCast.sol

Provides safe downcasting between integer types. Vyper's built-in `convert()`
already performs checked narrowing conversions (reverts on overflow/underflow),
so these are thin wrappers for API parity with v3-core.
"""


@internal
@pure
def to_uint160(y: uint256) -> uint160:
    """Cast uint256 → uint160, revert on overflow.

    Equivalent to Solidity: require((z = uint160(y)) == y)
    """
    z: uint160 = convert(y, uint160)
    assert convert(z, uint256) == y, "SafeCast: overflow"
    return z


@internal
@pure
def to_int128(y: int256) -> int128:
    """Cast int256 → int128, revert on overflow or underflow.

    Equivalent to Solidity: require((z = int128(y)) == y)
    """
    z: int128 = convert(y, int128)
    assert convert(z, int256) == y, "SafeCast: overflow"
    return z


@internal
@pure
def to_int256(y: uint256) -> int256:
    """Cast uint256 → int256, revert on overflow.

    Equivalent to Solidity: require(y < 2**255); z = int256(y)
    """
    assert y < (1 << 255), "SafeCast: overflow"
    return convert(y, int256)
