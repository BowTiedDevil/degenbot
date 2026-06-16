//! `FullMath`: 512-bit precision multiplication-division.
//!
//! Port of `FullMath.sol` from both Uniswap V3 and V4.
//!
//! In Solidity, these functions use `mulmod` and manual 512-bit decomposition
//! to avoid overflow. In Rust, we promote to `U512` for the intermediate
//! product, which avoids the entire decomposition algorithm.

use alloy::primitives::{U256, U512};

use crate::errors::ClMathError;

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
    let result = muldiv(a, b, denominator)?;

    // Round up if there's any remainder: (a*b) % d != 0
    let product = U512::from(a) * U512::from(b);
    let remainder = product % U512::from(denominator);
    if remainder.is_zero() {
        Ok(result)
    } else {
        result
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
