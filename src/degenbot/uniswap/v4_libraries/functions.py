def mulmod(
    x: int,
    y: int,
    k: int,
) -> int:
    """
    Returns (x*y)%k, as implemented by Yul.

    ref: https://docs.soliditylang.org/en/latest/yul.html
    """

    return 0 if k == 0 else (x * y) % k
