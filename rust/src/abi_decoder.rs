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

use crate::abi_types::AbiValue;
use crate::abi_types::cached::get_cached_types;
use crate::errors::AbiDecodeError;
use alloy::hex;
use alloy::primitives::Address;
use pyo3::{
    exceptions::{PyNotImplementedError, PyValueError},
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};

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
    values.pop().ok_or(AbiDecodeError::EmptyData)
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
/// use degenbot_rs::abi_types::{AbiType, AbiValue};
/// use degenbot_rs::abi_decoder::decode_for_types;
/// use alloy::primitives::U256;
///
/// let types = vec![AbiType::Uint(256), AbiType::Bool];
///
/// // Encode first, then decode
/// use degenbot_rs::abi_encoder::encode_for_types;
/// let values = vec![AbiValue::Uint(U256::from(42u64)), AbiValue::Bool(true)];
/// let encoded = encode_for_types(&types, &values)?;
///
/// let decoded = decode_for_types(&types, &encoded)?;
/// assert_eq!(decoded.len(), 2);
///
/// Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn decode_for_types(types: &[crate::abi_types::AbiType], data: &[u8]) -> Result<Vec<AbiValue>, AbiDecodeError> {
    if types.is_empty() {
        return Ok(Vec::new());
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    let cached = crate::abi_types::CachedAbiTypes::from_abi_types(types)?;
    cached.decode(data)
}

// =============================================================================
// Python conversion
// =============================================================================

/// Convert an `AbiValue` to a Python object.
fn abi_value_to_python<'py>(value: &AbiValue, py: Python<'py>, checksum: bool) -> PyResult<Bound<'py, PyAny>> {
    match value {
        AbiValue::Address(addr_bytes) => {
            if checksum {
                let addr = Address::from_slice(addr_bytes);
                Ok(PyString::new(py, &addr.to_string()).into_any())
            } else {
                let mut buf = [0u8; 42];
                buf[0] = b'0';
                buf[1] = b'x';
                hex::encode_to_slice(addr_bytes, &mut buf[2..]).map_err(|e| {
                    PyValueError::new_err(format!("Failed to encode address to hex: {e}"))
                })?;
                let addr_str = std::str::from_utf8(&buf).map_err(|e| {
                    PyValueError::new_err(format!("Invalid UTF-8 in hex address: {e}"))
                })?;
                Ok(PyString::new(py, addr_str).into_any())
            }
        }
        AbiValue::Bool(b) => Ok(PyBool::new(py, *b).to_owned().into_any()),
        AbiValue::FixedBytes(bytes) | AbiValue::Bytes(bytes) => {
            Ok(PyBytes::new(py, bytes).into_any())
        }
        AbiValue::Uint(n) => crate::alloy_py::u256_to_py(py, n),
        AbiValue::Int(n) => crate::alloy_py::i256_to_py(py, n),
        AbiValue::String(s) => Ok(PyString::new(py, s).into_any()),
        AbiValue::Array(values) => {
            let list = PyList::empty(py);
            for value in values {
                list.append(abi_value_to_python(value, py, checksum)?)?;
            }
            Ok(list.into_any())
        }
    }
}

/// Convert a slice of `AbiValue` to a Python list.
fn decoded_values_to_py_list<'py>(
    py: Python<'py>,
    values: &[AbiValue],
    checksum: bool,
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for value in values {
        let py_value = abi_value_to_python(value, py, checksum)?;
        list.append(py_value)?;
    }
    Ok(list)
}

// =============================================================================
// PyO3-exposed functions (thin wrappers)
// =============================================================================

/// Decode ABI-encoded data for multiple types.
///
/// # Arguments
///
/// * `types` - List of ABI type strings
/// * `data` - Raw ABI-encoded bytes
/// * `strict` - If true (default), performs strict validation
/// * `checksum` - If true (default), returns checksummed addresses
///
/// # Returns
///
/// A list of decoded Python values.
///
/// # Architecture
///
/// This PyO3-exposed function is a thin wrapper around the pure Rust `decode_rust`.
/// The decoding happens entirely without GIL, then results are converted to Python
/// objects in a single pass. This enables:
/// - Parallel decoding without GIL contention
/// - Pure Rust unit testing
/// - Clean separation of concerns
#[pyfunction]
#[pyo3(signature = (types, data, strict = true, checksum = true))]
pub fn decode(
    py: Python<'_>,
    types: &Bound<'_, PyList>,
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    // Extract type strings from Python list
    let type_strings: Vec<String> = types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;
    let type_refs: Vec<&str> = type_strings.iter().map(String::as_str).collect();

    let values = py.detach(|| decode_rust(&type_refs, data)).map_err(|e| {
        if matches!(e, AbiDecodeError::FixedPointNotImplemented) {
            PyNotImplementedError::new_err(format!("{e}"))
        } else {
            PyValueError::new_err(format!("{e}"))
        }
    })?;

    let list = decoded_values_to_py_list(py, &values, checksum)?;
    Ok(list.into())
}

/// Decode a single ABI value.
///
/// Convenience function for decoding a single value.
///
/// # Architecture
///
/// This PyO3-exposed function wraps `decode_single_rust` for consistent architecture.
#[pyfunction]
#[pyo3(signature = (abi_type, data, strict = true, checksum = true))]
pub fn decode_single(
    py: Python<'_>,
    abi_type: &str,
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    let value = py
        .detach(|| decode_single_rust(abi_type, data))
        .map_err(|e| {
            if matches!(e, AbiDecodeError::FixedPointNotImplemented) {
                PyNotImplementedError::new_err(format!("{e}"))
            } else {
                PyValueError::new_err(format!("{e}"))
            }
        })?;

    let py_value = abi_value_to_python(&value, py, checksum)?;
    Ok(py_value.unbind())
}

// =============================================================================
// Tests
// =============================================================================

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

    #[test]
    fn test_decode_uint256_rust() {
        let mut data = vec![0u8; 32];
        data[30] = 0x30;
        data[31] = 0x39;

        let result = decode_single_rust("uint256", &data).expect("should decode uint256");
        match result {
            AbiValue::Uint(n) => assert_eq!(n, U256::from(12345u64)),
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
                        AbiValue::Uint(n) => assert_eq!(*n, U256::from(i as u64 + 1)),
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
                    AbiValue::Uint(n) => assert_eq!(*n, U256::from(10u64)),
                    _ => panic!("Expected Uint"),
                }
                match &values[1] {
                    AbiValue::Uint(n) => assert_eq!(*n, U256::from(20u64)),
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
            AbiValue::Uint(n) => assert_eq!(*n, U256::from(42u64)),
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
            AbiValue::Int(n) => assert_eq!(n, I256::MINUS_ONE),
            _ => panic!("Expected Int variant"),
        }
    }

    #[test]
    fn test_abi_value_to_python_roundtrip() {
        #[allow(unsafe_code)]
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                let val = AbiValue::Uint(U256::from(123_456_789_u64));
                let py_val = abi_value_to_python(&val, py, true).expect("should convert to Python");
                let n: u64 = py_val.extract().expect("should extract as u64");
                assert_eq!(n, 123_456_789_u64);

                let val = AbiValue::Bool(true);
                let py_val = abi_value_to_python(&val, py, true).expect("should convert to Python");
                let b: bool = py_val.extract().expect("should extract as bool");
                assert!(b);

                let val = AbiValue::String("Hello".to_string());
                let py_val = abi_value_to_python(&val, py, true).expect("should convert to Python");
                let s: String = py_val.extract().expect("should extract as String");
                assert_eq!(s, "Hello");

                let val = AbiValue::Array(vec![
                    AbiValue::Uint(U256::from(1u64)),
                    AbiValue::Uint(U256::from(2u64)),
                ]);
                let py_val = abi_value_to_python(&val, py, true).expect("should convert to Python");
                let list: Vec<u64> = py_val.extract().expect("should extract as Vec<u64>");
                assert_eq!(list, vec![1, 2]);
            });
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
            AbiValue::Uint(n) => assert_eq!(*n, U256::from(42u64)),
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
        use crate::abi_types::cached::TYPE_CACHE;

        // Ensure cache starts empty
        TYPE_CACHE.lock().clear();

        // First call should populate cache
        let data = vec![0u8; 32];
        let _result1 = decode_single_rust("uint256", &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 1);

        // Second call should use cache (same key)
        let _result2 = decode_single_rust("uint256", &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 1); // Still 1, not 2

        // Different type should add to cache
        let _result3 = decode_single_rust("address", &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 2);
    }

    #[test]
    fn test_type_caching_multiple_types() {
        use crate::abi_types::cached::TYPE_CACHE;

        TYPE_CACHE.lock().clear();

        let mut data = Vec::new();
        data.extend(vec![0u8; 32]); // uint256
        data.extend(vec![0u8; 32]); // bool

        // First call with these types
        let _result1 = decode_rust(&["uint256", "bool"], &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 1);

        // Second call with same types should use cache
        let _result2 = decode_rust(&["uint256", "bool"], &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 1);

        // Different order is a different cache entry
        let _result3 = decode_rust(&["bool", "uint256"], &data).unwrap();
        assert_eq!(TYPE_CACHE.lock().len(), 2);
    }
}
