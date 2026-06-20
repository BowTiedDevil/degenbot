//! ABI decoding for Ethereum data.
//!
//! High-performance decoding of ABI-encoded data using alloy's `dyn_abi`.
//!
//! # Architecture
//!
//! This module uses a two-layer architecture:
//!
//! 1. **Pure Rust core**: `decode_rust()` functions that operate
//!    entirely without `PyO3` dependencies. This enables:
//!    - Unit testing without Python
//!    - Parallel decoding without GIL
//!    - Reuse in non-Python Rust code
//!
//! 2. **Thin `PyO3` wrapper**: `decode()` and `decode_single()` functions that convert
//!    `AbiValue` results to Python objects in a single pass.
//!
//! # Caching
//!
//! Type parsing is cached internally for repeated calls with the same type signatures.
//! This provides significant performance benefits when processing thousands of values
//! (e.g., decoding Transfer events from historical blocks).

use crate::abi_types::cached::get_cached_types;
use crate::abi_types::AbiValue;
use degenbot_core::errors::AbiDecodeError;

// =============================================================================
// Pure Rust decoding functions
// =============================================================================

/// Decode ABI-encoded data for multiple types (pure Rust).
///
/// This is the core decoding function with no Python dependencies.
/// Uses internal type parsing cache for performance.
///
/// # Arguments
///
/// * `types` - Slice of ABI type strings
/// * `data` - Raw ABI-encoded bytes
///
/// # Returns
///
/// A vector of `AbiValue` enums representing the decoded values.
///
/// # Errors
///
/// Returns `AbiDecodeError` if decoding fails.
pub fn decode_rust(types: &[&str], data: &[u8]) -> Result<Vec<AbiValue>, AbiDecodeError> {
    if types.is_empty() {
        return Err(AbiDecodeError::EmptyTypesList);
    }

    // Check for fixed-point types (not yet implemented)
    for ty in types {
        if ty.contains("fixed") || ty.contains("ufixed") {
            return Err(AbiDecodeError::FixedPointNotImplemented);
        }
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    // Use cached types for decoding
    let cached = get_cached_types(types)?;
    cached.decode(data)
}

/// Decode a single ABI value (pure Rust).
///
/// Convenience function for decoding a single value without Python dependencies.
/// Uses internal type parsing cache for performance.
///
/// # Panics
///
/// Panics if `cached.decode()` returns a different number of values than
/// the length check allows. This is a logic error and should never occur.
#[allow(clippy::missing_errors_doc)]
#[allow(clippy::missing_panics_doc)]
pub fn decode_single_rust(abi_type: &str, data: &[u8]) -> Result<AbiValue, AbiDecodeError> {
    if abi_type.contains("fixed") || abi_type.contains("ufixed") {
        return Err(AbiDecodeError::FixedPointNotImplemented);
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    // Use cached types for decoding
    let cached = get_cached_types(&[abi_type])?;
    let mut values = cached.decode(data)?;
    if values.len() != 1 {
        return Err(AbiDecodeError::InvalidLength(format!(
            "Expected 1 value from single-type decode, got {}",
            values.len()
        )));
    }
    // Length verified as 1 above; pop is infallible.
    #[allow(clippy::expect_used)]
    Ok(values.pop().expect("length verified above"))
}

/// Decode ABI-encoded data using pre-parsed `AbiType` values.
///
/// This is more efficient than `decode_rust()` because it avoids
/// string parsing for each type. Use this when you already have
/// `AbiType` instances (e.g., from `FunctionSignature::outputs`).
///
/// # Arguments
///
/// * `types` - Slice of `AbiType` values
/// * `data` - Raw ABI-encoded bytes
///
/// # Returns
///
/// A vector of `AbiValue` enums representing the decoded values.
///
/// # Errors
///
/// Returns `AbiDecodeError` if decoding fails.
///
/// # Example
///
/// ```
/// use degenbot_abi::abi_types::{AbiType, AbiValue};
/// use degenbot_abi::abi_decoder::decode_for_types;
/// use alloy::primitives::U256;
///
/// let types = vec![AbiType::Uint(256), AbiType::Bool];
///
/// // Encode first, then decode
/// use degenbot_abi::abi_encoder::encode_for_types;
/// let values = vec![AbiValue::Uint(U256::from(42u64), 256), AbiValue::Bool(true)];
/// let encoded = encode_for_types(&types, &values)?;
///
/// let decoded = decode_for_types(&types, &encoded)?;
/// assert_eq!(decoded.len(), 2);
///
/// Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn decode_for_types(
    types: &[crate::abi_types::AbiType],
    data: &[u8],
) -> Result<Vec<AbiValue>, AbiDecodeError> {
    if types.is_empty() {
        return Ok(Vec::new());
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    let cached = crate::abi_types::CachedAbiTypes::from_abi_types(types)?;
    cached.decode(data)
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::useless_vec,
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::cast_possible_truncation,
        clippy::unreadable_literal,
        clippy::needless_range_loop
    )]

    use super::*;
    use alloy::primitives::{I256, U256};
    use std::sync::Arc;

    #[test]
    fn test_decode_uint256_rust() {
        let mut data = vec![0u8; 32];
        data[30] = 0x30;
        data[31] = 0x39;

        let result = decode_single_rust("uint256", &data).expect("should decode uint256");
        match result {
            AbiValue::Uint(n, bits) => {
                assert_eq!(n, U256::from(12345u64));
                assert_eq!(bits, 256);
            }
            _ => panic!("Expected Uint variant"),
        }
    }

    #[test]
    fn test_decode_address_rust() {
        let mut data = vec![0u8; 32];
        for i in 0..20 {
            data[12 + i] = 0x10 + i as u8;
        }

        let result = decode_single_rust("address", &data).expect("should decode address");
        match result {
            AbiValue::Address(addr) => {
                for (i, byte) in addr.iter().enumerate() {
                    assert_eq!(*byte, 0x10 + i as u8);
                }
            }
            _ => panic!("Expected Address variant"),
        }
    }

    #[test]
    fn test_decode_bool_rust() {
        let mut data = vec![0u8; 32];
        data[31] = 1;

        let result = decode_single_rust("bool", &data).expect("should decode bool true");
        match result {
            AbiValue::Bool(b) => assert!(b),
            _ => panic!("Expected Bool variant"),
        }

        data[31] = 0;
        let result = decode_single_rust("bool", &data).expect("should decode bool false");
        match result {
            AbiValue::Bool(b) => assert!(!b),
            _ => panic!("Expected Bool variant"),
        }
    }

    #[test]
    fn test_decode_fixed_bytes_rust() {
        let data: Vec<u8> = (0..32).collect();

        let result = decode_single_rust("bytes32", &data).expect("should decode bytes32");
        match result {
            AbiValue::FixedBytes(bytes) => {
                assert_eq!(bytes.len(), 32);
                for i in 0..32 {
                    assert_eq!(bytes[i], i as u8);
                }
            }
            _ => panic!("Expected FixedBytes variant"),
        }
    }

    #[test]
    fn test_decode_dynamic_bytes_rust() {
        let mut data = vec![0u8; 64];
        data[31] = 32;
        data[63] = 3;
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        data.extend_from_slice(&[
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);

        let result = decode_single_rust("bytes", &data).expect("should decode bytes");
        match result {
            AbiValue::Bytes(bytes) => {
                assert_eq!(bytes, vec![0xAA, 0xBB, 0xCC]);
            }
            _ => panic!("Expected Bytes variant"),
        }
    }

    #[test]
    fn test_decode_string_rust() {
        let hello = b"Hello, World!";
        let mut data = vec![0u8; 64];
        data[31] = 32;
        data[63] = u8::try_from(hello.len()).expect("length fits in u8");
        data.extend_from_slice(hello);
        let padding = (32 - (hello.len() % 32)) % 32;
        data.extend(std::iter::repeat_n(0, padding));

        let result = decode_single_rust("string", &data).expect("should decode string");
        match result {
            AbiValue::String(s) => {
                assert_eq!(s, "Hello, World!");
            }
            _ => panic!("Expected String variant"),
        }
    }

    #[test]
    fn test_decode_static_array_rust() {
        let mut data = Vec::new();
        for i in 1..=3 {
            let mut word = vec![0u8; 32];
            word[31] = i;
            data.extend(word);
        }

        let result = decode_single_rust("uint256[3]", &data).expect("should decode uint256[3]");
        match result {
            AbiValue::Array(values) => {
                assert_eq!(values.len(), 3);
                for (i, val) in values.iter().enumerate() {
                    match val {
                        AbiValue::Uint(n, bits) => {
                            assert_eq!(*n, U256::from(i as u64 + 1));
                            assert_eq!(*bits, 256);
                        }
                        _ => panic!("Expected Uint in array"),
                    }
                }
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_decode_dynamic_array_rust() {
        let mut data = vec![0u8; 64];
        data[31] = 32;
        data[63] = 2;

        let mut word1 = vec![0u8; 32];
        word1[31] = 10;
        data.extend(word1);

        let mut word2 = vec![0u8; 32];
        word2[31] = 20;
        data.extend(word2);

        let result = decode_single_rust("uint256[]", &data).expect("should decode uint256[]");
        match result {
            AbiValue::Array(values) => {
                assert_eq!(values.len(), 2);
                match &values[0] {
                    AbiValue::Uint(n, bits) => {
                        assert_eq!(*n, U256::from(10u64));
                        assert_eq!(*bits, 256);
                    }
                    _ => panic!("Expected Uint"),
                }
                match &values[1] {
                    AbiValue::Uint(n, bits) => {
                        assert_eq!(*n, U256::from(20u64));
                        assert_eq!(*bits, 256);
                    }
                    _ => panic!("Expected Uint"),
                }
            }
            _ => panic!("Expected Array variant"),
        }
    }

    #[test]
    fn test_decode_multiple_values_rust() {
        let mut data = Vec::new();

        let mut word1 = vec![0u8; 32];
        word1[31] = 42;
        data.extend(word1);

        let mut word2 = vec![0u8; 32];
        word2[31] = 1;
        data.extend(word2);

        let mut word3 = vec![0u8; 32];
        word3[31] = 1;
        data.extend(word3);

        let result =
            decode_rust(&["uint256", "bool", "address"], &data).expect("should decode multiple");
        assert_eq!(result.len(), 3);

        match &result[0] {
            AbiValue::Uint(n, bits) => {
                assert_eq!(*n, U256::from(42u64));
                assert_eq!(*bits, 256);
            }
            _ => panic!("Expected Uint"),
        }
        match &result[1] {
            AbiValue::Bool(b) => assert!(*b),
            _ => panic!("Expected Bool"),
        }
        match &result[2] {
            AbiValue::Address(addr) => {
                assert_eq!(addr[19], 1);
            }
            _ => panic!("Expected Address"),
        }
    }

    #[test]
    fn test_decode_int256_negative_rust() {
        let data = vec![0xFFu8; 32];

        let result = decode_single_rust("int256", &data).expect("should decode int256");
        match result {
            AbiValue::Int(n, bits) => {
                assert_eq!(n, I256::MINUS_ONE);
                assert_eq!(bits, 256);
            }
            _ => panic!("Expected Int variant"),
        }
    }

    // =========================================================================
    // decode_for_types tests (pre-parsed AbiType)
    // =========================================================================

    #[test]
    fn test_decode_for_types_basic() {
        use crate::abi_types::AbiType;

        // Encode uint256 and bool
        let mut data = Vec::new();
        let mut word1 = vec![0u8; 32];
        word1[31] = 42;
        data.extend(word1);

        let mut word2 = vec![0u8; 32];
        word2[31] = 1;
        data.extend(word2);

        let types = vec![AbiType::Uint(256), AbiType::Bool];
        let decoded = decode_for_types(&types, &data).expect("should decode");

        assert_eq!(decoded.len(), 2);
        match &decoded[0] {
            AbiValue::Uint(n, bits) => {
                assert_eq!(*n, U256::from(42u64));
                assert_eq!(*bits, 256);
            }
            _ => panic!("Expected Uint"),
        }
        match &decoded[1] {
            AbiValue::Bool(b) => assert!(*b),
            _ => panic!("Expected Bool"),
        }
    }

    #[test]
    fn test_decode_for_types_matches_decode_rust() {
        use crate::abi_types::AbiType;

        // Create test data
        let mut data = Vec::new();
        let mut word1 = vec![0u8; 32];
        word1[31] = 42;
        data.extend(word1);

        let mut word2 = vec![0u8; 32];
        word2[31] = 1;
        data.extend(word2);

        let mut word3 = vec![0u8; 32];
        word3[19] = 1;
        data.extend(word3);

        // Compare both methods
        let types_str = vec!["uint256", "bool", "address"];
        let types_abi = vec![AbiType::Uint(256), AbiType::Bool, AbiType::Address];

        let decoded_rust = decode_rust(&types_str, &data).expect("should decode");
        let decoded_for_types = decode_for_types(&types_abi, &data).expect("should decode");

        assert_eq!(decoded_rust, decoded_for_types);
    }

    #[test]
    fn test_decode_for_types_empty() {
        use crate::abi_types::AbiType;

        let types: Vec<AbiType> = vec![];
        let decoded = decode_for_types(&types, &[]).expect("empty types should succeed");
        assert!(decoded.is_empty());
    }

    // =========================================================================
    // Caching tests (verify the shared cache works for decoding)
    // =========================================================================

    #[test]
    fn test_type_caching() {
        use crate::abi_types::cached::get_cached_types;

        // Two calls with the same key return the same Arc allocation
        let cached1 = get_cached_types(&["uint256"]).unwrap();
        let cached2 = get_cached_types(&["uint256"]).unwrap();
        assert!(Arc::ptr_eq(&cached1, &cached2));

        // Different key returns a different Arc
        let cached3 = get_cached_types(&["address"]).unwrap();
        assert!(!Arc::ptr_eq(&cached1, &cached3));
    }

    #[test]
    fn test_type_caching_multiple_types() {
        use crate::abi_types::cached::get_cached_types;

        // Same multi-type key returns same Arc
        let cached1 = get_cached_types(&["uint256", "bool"]).unwrap();
        let cached2 = get_cached_types(&["uint256", "bool"]).unwrap();
        assert!(Arc::ptr_eq(&cached1, &cached2));

        // Different order is a different key
        let cached3 = get_cached_types(&["bool", "uint256"]).unwrap();
        assert!(!Arc::ptr_eq(&cached1, &cached3));
    }
}
