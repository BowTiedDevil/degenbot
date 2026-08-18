"""Uniswap V3 pure helper functions for tick and price conversion."""

Q96 = 2**96


def v3_virtual_reserves(
    liquidity: int,
    sqrt_price_x96: int,
    *,
    zero_for_one: bool,
) -> tuple[int, int]:
    """V3 virtual reserves.

    Returns:
        A tuple of (reserve_in, reserve_out) virtual reserves.

    """
    x_virtual = liquidity * Q96 * Q96 // sqrt_price_x96
    y_virtual = liquidity * sqrt_price_x96
    if zero_for_one:
        return x_virtual, y_virtual
    return y_virtual, x_virtual
