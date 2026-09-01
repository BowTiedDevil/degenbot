//! `FullMath`: 512-bit precision multiplication-division.
//!
//! Port of `FullMath.sol` from both Uniswap V3 and V4.
//!
//! In Solidity, these functions use `mulmod` and manual 512-bit decomposition
//! to avoid overflow. In Rust, we promote to `U512` for the intermediate
//! product, which avoids the entire decomposition algorithm.

use alloy::primitives::{U256, U512};

use degenbot_core::errors::ClMathError;

/// Byte-identical fast paths shared by [`muldiv`] / [`muldiv_rounding_up`].
///
/// Returns `None` when no fast path applies (caller falls back to the U512
/// product + U512 division). Both paths are exact-integer rewrites:
/// - power-of-two denominator: `(a*b) >> s` plus a remainder test for the
///   rounding-up variant — division by 2^s is a shift;
/// - narrow operands (`a`, `b` ≤ 2^128 − 1): the product fits `U256`, so the
///   division runs at 256/256 bits instead of 512/512.
enum FastDiv {
    Floor(U256),
    Ceil(U256),
}

#[inline]
fn fast_muldiv(a: U256, b: U256, denominator: U256, rounding_up: bool) -> Option<FastDiv> {
    // Power-of-two denominator: the wide product shifted by `s` is exactly
    // the quotient; the high bits of `s` form the remainder (for the ceil
    // variant) and the same result bound governs the overflow check.
    if denominator.count_ones() == 1 {
        let shift = denominator.trailing_zeros();
        let product = U512::from(a) * U512::from(b);
        let quotient = product >> shift;
        if quotient > U512::from(U256::MAX) {
            // Overflow must surface identically to the generic path. The
            // rounded-up quotient can only distinguish itself from the floor
            // by +1 when the discarded bits are nonzero; if the floor already
            // exceeds U256::MAX the ceil does too.
            return None;
        }
        let floor = quotient.to::<U256>();
        if rounding_up
            && (product & (U512::from(1u8) << shift).wrapping_sub(U512::from(1u8))) != U512::ZERO
        {
            return floor.checked_add(U256::from(1u8)).map(FastDiv::Ceil);
        }
        return Some(FastDiv::Floor(floor));
    }
    // Narrow operands: 256/256 division instead of 512/512.
    if a <= U256::from(u128::MAX) && b <= U256::from(u128::MAX) {
        let product = a.wrapping_mul(b); // < 2^256: both factors < 2^128
        let quotient = product / denominator;
        if rounding_up {
            let remainder = product % denominator;
            if remainder.is_zero() {
                return Some(FastDiv::Floor(quotient));
            }
            return quotient.checked_add(U256::from(1u8)).map(FastDiv::Ceil);
        }
        return Some(FastDiv::Floor(quotient));
    }
    None
}

/// Compute `floor(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// - [`ClMathError::DivisionByZero`] if `denominator` is zero.
/// - [`ClMathError::Uint256Overflow`] if the result exceeds `U256::MAX`.
#[must_use = "computation result should be used"]
pub fn muldiv(a: U256, b: U256, denominator: U256) -> Result<U256, ClMathError> {
    if denominator.is_zero() {
        return Err(ClMathError::DivisionByZero);
    }

    if let Some(FastDiv::Floor(result)) = fast_muldiv(a, b, denominator, false) {
        return Ok(result);
    }

    // Promote to U512 to hold the full 512-bit product without overflow.
    let product = U512::from(a) * U512::from(b);
    let result = product / U512::from(denominator);

    // The Solidity contract requires the result to fit in uint256.
    if result > U512::from(U256::MAX) {
        return Err(ClMathError::Uint256Overflow);
    }

    Ok(result.to::<U256>())
}

/// Compute `ceil(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// - [`ClMathError::DivisionByZero`] if `denominator` is zero.
/// - [`ClMathError::Uint256Overflow`] if the result exceeds `U256::MAX`.
#[must_use = "computation result should be used"]
pub fn muldiv_rounding_up(a: U256, b: U256, denominator: U256) -> Result<U256, ClMathError> {
    // Single-pass: product once; div + mod from the same U512 operands
    // (previously muldiv() re-multiplied to obtain the remainder).
    if denominator.is_zero() {
        return Err(ClMathError::DivisionByZero);
    }
    if let Some(fast) = fast_muldiv(a, b, denominator, true) {
        return Ok(match fast {
            FastDiv::Floor(v) | FastDiv::Ceil(v) => v,
        });
    }
    let product = U512::from(a) * U512::from(b);
    let den = U512::from(denominator);
    let (raw_result, remainder) = product.div_rem(den);
    if raw_result > U512::from(U256::MAX) {
        return Err(ClMathError::Uint256Overflow);
    }
    let floor = raw_result.to::<U256>();
    if remainder.is_zero() {
        Ok(floor)
    } else {
        floor
            .checked_add(U256::from(1u8))
            .ok_or(ClMathError::Uint256Overflow)
    }
}

/// Compute `(a * b) % k` — the Yul `mulmod` builtin.
///
/// Returns zero if `k` is zero, matching Yul semantics.
#[must_use]
pub fn mulmod(a: U256, b: U256, k: U256) -> U256 {
    if k.is_zero() {
        return U256::ZERO;
    }
    let product = U512::from(a) * U512::from(b);
    let result = product % U512::from(k);
    // mulmod fits in uint256 because (a*b) mod k ≤ k-1 ≤ U256::MAX
    result.to::<U256>()
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_muldiv_basic() {
        assert_eq!(
            muldiv(U256::from(3u64), U256::from(4u64), U256::from(2u64)).unwrap(),
            U256::from(6u64)
        );
    }

    #[test]
    fn test_muldiv_division_by_zero() {
        assert!(matches!(
            muldiv(U256::from(1u64), U256::from(2u64), U256::ZERO),
            Err(ClMathError::DivisionByZero)
        ));
    }

    #[test]
    fn test_muldiv_rounding_up_no_remainder() {
        assert_eq!(
            muldiv_rounding_up(U256::from(6u64), U256::from(4u64), U256::from(2u64)).unwrap(),
            U256::from(12u64)
        );
    }

    #[test]
    fn test_muldiv_rounding_up_with_remainder() {
        // ceil(5*3/2) = ceil(7.5) = 8
        assert_eq!(
            muldiv_rounding_up(U256::from(5u64), U256::from(3u64), U256::from(2u64)).unwrap(),
            U256::from(8u64)
        );
    }

    #[test]
    fn test_mulmod_basic() {
        assert_eq!(
            mulmod(U256::from(5u64), U256::from(3u64), U256::from(7u64)),
            U256::from(1u64) // 15 % 7 = 1
        );
    }

    #[test]
    fn test_mulmod_zero_k() {
        assert_eq!(
            mulmod(U256::from(5u64), U256::from(3u64), U256::ZERO),
            U256::ZERO
        );
    }
}
