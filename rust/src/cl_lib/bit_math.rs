//! `BitMath`: find most/least significant bit positions.
//!
//! Port of `BitMath.sol` from both Uniswap V3 and V4.
//!
//! The V3 Python implementation uses string search (slow), while V4 uses
//! `.bit_length()`. The Rust implementation uses native bit operations.

use alloy::primitives::U256;

use crate::errors::ClMathError;

/// Return the index of the most significant bit set in `x`.
///
/// Equivalent to Solidity `BitMath.mostSignificantBit`.
///
/// # Errors
///
/// Returns [`ClMathError::ZeroInput`] if `x` is zero.
#[must_use = "result should be used"]
pub fn most_significant_bit(x: U256) -> Result<u8, ClMathError> {
    if x.is_zero() {
        return Err(ClMathError::ZeroInput);
    }
    // U256 is 256 bits; bit_length() returns 1..=256 for non-zero values.
    // MSB position = bit_length - 1. Subtract before cast: result is 0..=255,
    // which fits in u8 without truncation.
    #[allow(clippy::cast_possible_truncation)]
    Ok((x.bit_len() - 1) as u8)
}

/// Return the index of the least significant bit set in `x`.
///
/// Equivalent to Solidity `BitMath.leastSignificantBit`.
///
/// # Errors
///
/// Returns [`ClMathError::ZeroInput`] if `x` is zero.
#[must_use = "result should be used"]
pub fn least_significant_bit(x: U256) -> Result<u8, ClMathError> {
    if x.is_zero() {
        return Err(ClMathError::ZeroInput);
    }
    // Trailing zeros count gives the LSB position directly.
    // Safe: trailing_zeros() returns 0..=255 for non-zero U256, which fits in u8.
    #[allow(clippy::cast_possible_truncation)]
    Ok(x.trailing_zeros() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_msb_basic() {
        assert_eq!(most_significant_bit(U256::from(1u64)), Ok(0));
        assert_eq!(most_significant_bit(U256::from(2u64)), Ok(1));
        assert_eq!(most_significant_bit(U256::from(3u64)), Ok(1));
        assert_eq!(most_significant_bit(U256::from(256u64)), Ok(8));
    }

    #[test]
    fn test_lsb_basic() {
        assert_eq!(least_significant_bit(U256::from(1u64)), Ok(0));
        assert_eq!(least_significant_bit(U256::from(2u64)), Ok(1));
        assert_eq!(least_significant_bit(U256::from(6u64)), Ok(1)); // 0b110
        assert_eq!(least_significant_bit(U256::from(256u64)), Ok(8));
    }

    #[test]
    fn test_zero_input() {
        assert!(matches!(most_significant_bit(U256::ZERO), Err(ClMathError::ZeroInput)));
        assert!(matches!(least_significant_bit(U256::ZERO), Err(ClMathError::ZeroInput)));
    }
}
