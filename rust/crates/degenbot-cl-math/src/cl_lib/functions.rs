//! Helper functions: mulmod, compress, safe casts.
//!
//! These are small utility functions used across the CL math libraries.
//! They correspond to the Solidity Yul builtins and `OpenZeppelin` safe-cast
//! helpers.

use alloy::primitives::{I256, U256, U512};

use degenbot_core::errors::ClMathError;

/// Maximum value of uint160.
const MAX_UINT160: U256 = U256::from_limbs([
    0xffff_ffff_ffff_ffff_u64,
    0xffff_ffff_ffff_ffff_u64,
    0xffff_ffff_ffff_ffff_u64,
    0x0000_0000_0000_0000_u64,
]);

/// Compute `(x * y) % k`, matching Yul `mulmod` semantics.
///
/// Returns zero if `k` is zero (matching V4/Yul behavior). The V3 Solidity
/// contract reverts on division by zero instead — callers should check if
/// strict V3 behavior is needed.
#[must_use]
pub fn mulmod(x: U256, y: U256, k: U256) -> U256 {
    if k.is_zero() {
        return U256::ZERO;
    }
    let product = U512::from(x) * U512::from(y);
    let result = product % U512::from(k);
    // mulmod output fits in uint256 because (x*y) mod k < k ≤ U256::MAX
    result.to()
}

/// Compress a tick by its spacing, rounding toward negative infinity.
///
/// Matches Python's `//` operator and Solidity's division for positive
/// values. For negative values, uses [`i32::div_euclid`] which floors
/// toward negative infinity.
#[must_use]
pub const fn compress(tick: i32, tick_spacing: i32) -> i32 {
    tick.div_euclid(tick_spacing)
}

/// Safe cast to `uint160`. Returns an error if the value exceeds `uint160` max.
///
/// # Errors
///
/// Returns [`ClMathError::SafeCastOverflow`] if `x` exceeds `uint160` max.
#[must_use = "safe-cast results should be checked"]
pub fn to_uint160(x: U256) -> Result<U256, ClMathError> {
    if x > MAX_UINT160 {
        return Err(ClMathError::SafeCastOverflow);
    }
    Ok(x)
}

/// Safe cast to `int128`. Returns an error if the value is out of range.
///
/// # Errors
///
/// Returns [`ClMathError::SafeCastOverflow`] if `x` doesn't fit in `int128`.
#[must_use = "safe-cast results should be checked"]
pub fn to_int128(x: I256) -> Result<i128, ClMathError> {
    // Check if the I256 value fits in i128 by extracting bytes and checking sign extension
    let bytes = x.to_be_bytes::<32>();
    // If any of the high 16 bytes differ from the sign bit, it's out of range
    let sign_byte = bytes[0];
    let high_all_same =
        bytes[0..16].iter().all(|&b| b == sign_byte) && (sign_byte == 0x00 || sign_byte == 0xFF);
    if !high_all_same {
        return Err(ClMathError::SafeCastOverflow);
    }
    let low_bytes: [u8; 16] = bytes[16..32].try_into().unwrap_or([0u8; 16]);
    Ok(i128::from_be_bytes(low_bytes))
}

/// Safe cast to `int256`. This is essentially a no-op validation function
/// used for overflow checks on arithmetic results in the Python code.
///
/// # Errors
///
/// Currently never errors — `I256` is already the target type.
#[must_use = "safe-cast results should be checked"]
pub const fn to_int256(x: I256) -> Result<I256, ClMathError> {
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mulmod_basic() {
        assert_eq!(
            mulmod(U256::from(5u64), U256::from(3u64), U256::from(7u64)),
            U256::from(1u64)
        );
    }

    #[test]
    fn test_mulmod_zero_k() {
        assert_eq!(
            mulmod(U256::from(5u64), U256::from(3u64), U256::ZERO),
            U256::ZERO
        );
    }

    #[test]
    fn test_compress() {
        assert_eq!(compress(100, 60), 1);
        assert_eq!(compress(-100, 60), -2);
        assert_eq!(compress(0, 60), 0);
    }

    #[test]
    fn test_to_uint160_valid() {
        assert_eq!(to_uint160(U256::from(100u64)).unwrap(), U256::from(100u64));
    }

    #[test]
    fn test_to_uint160_overflow() {
        assert!(to_uint160(U256::MAX).is_err());
    }
}
