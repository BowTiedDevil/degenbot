"""
Port of UniswapV3 FullMath.sol.

Source: contracts/libraries/FullMath.sol
  https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/FullMath.sol

Contains 512-bit math functions that handle "phantom overflow" —
multiplication and division where an intermediate value overflows 256 bits
without any loss of precision.

The Solidity original uses extensive inline assembly for:
1. 512-bit multiply: mulmod + mul to get [prod1, prod0]
2. 512-by-256 division: power-of-2 factoring + Newton-Raphson modular inverse

This Vyper port reimplements the same algorithms in pure Vyper (no assembly),
using uint256_mulmod() for the 512-bit multiply decomposition and
unsafe_mul/unsafe_sub for the modular arithmetic in the Newton-Raphson
inverse computation.
"""


@internal
@pure
def mul_div(a: uint256, b: uint256, denominator: uint256) -> uint256:
    """Calculates floor(a×b÷denominator) with full 512-bit precision.

    Throws if result overflows uint256 or denominator == 0.
    Handles "phantom overflow" where a×b exceeds 2^256 but the final
    result fits in 256 bits.

    Credit: Remco Bloemen (MIT license) — https://xn--2-umb.com/21/muldiv
    """
    assert denominator > 0, "FullMath: division by zero"

    # ── 512-bit multiply: [prod1 prod0] = a * b ──
    # prod0 = (a * b) % 2^256   (least significant 256 bits)
    # prod1 = (a * b) / 2^256   (most significant 256 bits)
    #
    # In Solidity assembly:
    #   let mm := mulmod(a, b, not(0))      # mm = (a*b) % (2^256 - 1)
    #   prod0 := mul(a, b)                   # prod0 = (a*b) % 2^256
    #   prod1 := sub(sub(mm, prod0), lt(mm, prod0))
    #
    # The carry (prod1) is derived from mm and prod0:
    #   if mm >= prod0: carry = mm - prod0       (no wrap happened)
    #   if mm <  prod0: carry = mm - prod0 - 1  (mul wrapped, mulmod didn't)

    mm: uint256 = uint256_mulmod(a, b, max_value(uint256))  # (a*b) % (2^256-1)
    prod0: uint256 = unsafe_mul(a, b)                        # (a*b) % 2^256  (wrapping mul)
    prod1: uint256 = empty(uint256)

    # Compute prod1 = carry from the 512-bit multiply
    if mm >= prod0:
        prod1 = mm - prod0
    else:
        prod1 = unsafe_sub(mm, prod0) - 1  # mm - prod0 underflows → unsafe_sub

    # ── Handle non-overflow cases (prod1 == 0) ──
    if prod1 == 0:
        return prod0 // denominator

    # ── Make sure result fits in uint256 ──
    assert denominator > prod1, "FullMath: overflow"

    # ── 512-by-256 division ──

    # Make division exact by subtracting remainder from [prod1 prod0]
    remainder: uint256 = uint256_mulmod(a, b, denominator)

    # Subtract remainder from the 512-bit number [prod1 prod0]
    if remainder > prod0:
        prod1 = unsafe_sub(prod1, 1)
    prod0 = unsafe_sub(prod0, remainder)

    # Factor powers of 2 out of denominator
    # Compute largest power of two divisor of denominator (always >= 1)
    twos: uint256 = unsafe_sub(0, denominator) & denominator  # (-d) & d

    # Divide denominator by power of two
    denominator = denominator // twos

    # Divide [prod1 prod0] by the factors of two
    prod0 = prod0 // twos

    # Shift in bits from prod1 into prod0.
    # twos becomes 2^256 / twos.
    # In Solidity: twos := add(div(sub(0, twos), twos), 1)
    twos = unsafe_add(unsafe_sub(0, twos) // twos, 1)
    prod0 = prod0 | unsafe_mul(prod1, twos)

    # ── Invert denominator mod 2^256 via Newton-Raphson ──
    # denominator is now odd (powers of 2 removed), so it has an inverse.
    # Inverse is computed mod 2^256 using Newton-Raphson (Hensel lifting),
    # doubling correct bits in each step: seed → 4→8→16→32→64→128→256 bits.
    #
    # In Solidity: inv *= 2 - denominator * inv   (all arithmetic wraps mod 2^256)
    # In Vyper: must use unsafe_mul and unsafe_sub for wrapping arithmetic.

    inv: uint256 = unsafe_mul(3, denominator) ^ 2  # 4-bit seed (wrapping mul)

    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 8 bits
    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 16 bits
    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 32 bits
    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 64 bits
    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 128 bits
    inv = unsafe_mul(inv, unsafe_sub(2, unsafe_mul(denominator, inv)))   # → 256 bits

    # Exact division via modular inverse: result = prod0 * inv mod 2^256
    return unsafe_mul(prod0, inv)


@internal
@pure
def mul_div_rounding_up(a: uint256, b: uint256, denominator: uint256) -> uint256:
    """Calculates ceil(a×b÷denominator) with full 512-bit precision.

    Throws if result overflows uint256 or denominator == 0.
    """
    result: uint256 = self.mul_div(a, b, denominator)
    if uint256_mulmod(a, b, denominator) > 0:
        assert result < max_value(uint256), "FullMath: overflow"
        result = unsafe_add(result, 1)
    return result
