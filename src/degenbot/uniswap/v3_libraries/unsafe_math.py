def div_rounding_up(x: int, y: int) -> int:
    """
    Perform an x//y floored division, rounding up any remainder.

    ref: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/UnsafeMath.sol
    """

    # x and y are uint256 values, so negative value floor division workarounds are unnecessary
    return x // y + (x % y > 0)
