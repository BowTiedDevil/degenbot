//! `LiquidityMath`: overflow-checked liquidity delta.
//!
//! Port of `LiquidityMath.sol` from both Uniswap V3 and V4.
//!
//! Both versions check that `x + y` fits in a `uint128`. The V3 Solidity
//! contract uses inline Yul with overflow tricks; the V4 contract uses a
//! simpler range check. In Rust, we just check the bounds directly.

use alloy::primitives::{I256, U256};

use crate::errors::ClMathError;

/// Add a signed delta `y` to `x`, checking that the result fits in `uint128`.
///
/// `x` must be in `[0, 2^128 - 1]` (uint128) and `y` in `[-2^127, 2^127 - 1]` (int128).
/// The returned value is guaranteed to be in `[0, 2^128 - 1]`.
///
/// # Errors
///
/// Returns [`ClMathError::SafeCastOverflow`] if the result overflows `uint128`.
#[must_use]
pub fn add_delta(x: u128, y: i128) -> Result<u128, ClMathError> {
    // Promote to i256 to avoid overflow when x is near u128::MAX
    // (matching Solidity: int256 z = int256(uint256(x)) + y)
    let x_u256 = U256::from(x);
    let x_i256 = I256::from_raw(x_u256); // Zero-extends correctly since x fits in u128
    let y_i256 = I256::from_be_bytes::<32>({
        let b = y.to_be_bytes();
        let mut padded = [0u8; 32];
        padded.fill(if y < 0 { 0xFF } else { 0x00 });
        padded[16..32].copy_from_slice(&b);
        padded
    });
    let result = x_i256.checked_add(y_i256).ok_or(ClMathError::SafeCastOverflow)?;
    // Result must be in [0, u128::MAX]
    let max_u128_i256 = I256::from_raw(U256::from(u128::MAX));
    if result < I256::ZERO || result > max_u128_i256 {
        return Err(ClMathError::SafeCastOverflow);
    }
    // Extract lower 128 bits as u128
    let result_bytes = result.to_be_bytes::<32>();
    Ok(u128::from_be_bytes(result_bytes[16..32].try_into().unwrap_or([0u8; 16])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_delta_basic() {
        assert_eq!(add_delta(100, 50), Ok(150));
        assert_eq!(add_delta(100, -50), Ok(50));
    }

    #[test]
    fn test_add_delta_underflow() {
        assert!(add_delta(10, -20).is_err());
    }

    #[test]
    fn test_add_delta_zero() {
        assert_eq!(add_delta(0, 0), Ok(0));
        assert_eq!(add_delta(100, 0), Ok(100));
    }
}
