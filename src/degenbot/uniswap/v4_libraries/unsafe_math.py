"""Uniswap V4 UnsafeMath: rounding-up division."""


def div_rounding_up(
    x: int,
    y: int,
) -> int:
    """Calculate ceil(x/y).

    @dev division by 0 will return 0, and should be checked externally.
    """
    return 0 if y == 0 else x // y + int(x % y > 0)


def simple_mul_div(a: int, b: int, denominator: int) -> int:
    """Calculate floor((a*b)/denominator)).

    @dev division by 0 will return 0, and should be checked externally
    """
    return 0 if denominator == 0 else (a * b) // denominator
