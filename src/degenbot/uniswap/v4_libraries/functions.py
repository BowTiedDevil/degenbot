"""Uniswap V4 pure helper functions for mulmod and encoding."""


def mulmod(
    x: int,
    y: int,
    k: int,
) -> int:
    """Return (x*y)%k, as implemented by Yul.

    Returns:
        The result of (x*y) mod k, or 0 if k is zero.

    ref: https://docs.soliditylang.org/en/latest/yul.html

    """
    return 0 if k == 0 else (x * y) % k
