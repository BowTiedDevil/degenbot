# @notice Returns ceil(x / y)
# @dev division by 0 will return 0, and should be checked externally
# @param x The dividend
# @param y The divisor
# @return z The quotient, ceil(x / y)
def div_rounding_up(x: int, y: int) -> int:
    return (0 if y == 0 else x // y) + ((0 if y == 0 else x % y) > 0)


# @notice Calculates floor(a×b÷denominator)
# @dev division by 0 will return 0, and should be checked externally
# @param a The multiplicand
# @param b The multiplier
# @param denominator The divisor
# @return result The 256-bit result, floor(a×b÷denominator)
def simple_mul_div(a: int, b: int, denominator: int) -> int:
    return 0 if denominator == 0 else ((a * b) // denominator)
