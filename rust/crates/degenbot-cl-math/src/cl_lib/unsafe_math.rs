//! `UnsafeMath`: rounding-up division and simple mul-div.
//!
//! Port of `UnsafeMath.sol` from both Uniswap V3 and V4.

use alloy::primitives::{U256, U512};

/// Compute `ceil(x / y)` — division that rounds up any remainder.
///
/// Returns zero if `y` is zero (should be validated by caller), matching
/// the V4 Solidity convention.
#[must_use]
pub fn div_rounding_up(x: U256, y: U256) -> U256 {
    if y.is_zero() {
        return U256::ZERO;
    }
    let quotient = x / y;
    let remainder = x % y;
    if remainder.is_zero() {
        quotient
    } else {
        quotient + U256::from(1u8)
    }
}

/// Compute `floor(a * b / denominator)` without overflow checks.
///
/// Returns zero if `denominator` is zero (should be validated by caller).
#[must_use]
pub fn simple_mul_div(a: U256, b: U256, denominator: U256) -> U256 {
    if denominator.is_zero() {
        return U256::ZERO;
    }
    let product = U512::from(a) * U512::from(b);
    (product / U512::from(denominator)).to()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_div_rounding_up_exact() {
        assert_eq!(
            div_rounding_up(U256::from(6u64), U256::from(3u64)),
            U256::from(2u64)
        );
    }

    #[test]
    fn test_div_rounding_up_with_remainder() {
        assert_eq!(
            div_rounding_up(U256::from(7u64), U256::from(3u64)),
            U256::from(3u64) // ceil(7/3) = 3
        );
    }

    #[test]
    fn test_div_rounding_up_zero_divisor() {
        assert_eq!(div_rounding_up(U256::from(1u64), U256::ZERO), U256::ZERO);
    }
}
