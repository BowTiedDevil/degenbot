//! ABI decoding for Ethereum data.
//!
//! High-performance decoding of ABI-encoded data using alloy's `dyn_abi`.
//!
//! # Architecture
//!
//! This module uses a two-layer architecture:
//!
//! 1. **Pure Rust core**: `DecodedValue` enum and `decode_rust()` functions that operate
//!    entirely without `PyO3` dependencies. This enables:
//!    - Unit testing without Python
//!    - Parallel decoding without GIL
//!    - Reuse in non-Python Rust code
//!
//! 2. **Thin `PyO3` wrapper**: `decode()` and `decode_single()` functions that convert
//!    `DecodedValue` results to Python objects in a single pass.

use crate::errors::AbiDecodeError;
use alloy::dyn_abi::DynSolType;
use alloy::hex;
use alloy::primitives::Address;
use num_bigint::{BigInt, BigUint};
use pyo3::{
    exceptions::{PyNotImplementedError, PyValueError},
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};

// =============================================================================
// DecodedValue - Pure Rust representation of decoded ABI values
// =============================================================================

/// Represents a decoded ABI value in pure Rust.
///
/// This enum captures all possible ABI types without any Python dependencies,
/// enabling pure Rust testing and GIL-free processing.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedValue {
    /// Ethereum address (20 bytes)
    Address([u8; 20]),
    /// Boolean value
    Bool(bool),
    /// Fixed-size bytes (bytes1-bytes32)
    FixedBytes(Vec<u8>),
    /// Dynamic bytes
    Bytes(Vec<u8>),
    /// Unsigned integer
    Uint(BigUint),
    /// Signed integer
    Int(BigInt),
    /// String
    String(String),
    /// Array of values
    Array(Vec<Self>),
}

impl DecodedValue {
    /// Convert a `DynSolValue` from alloy to `DecodedValue`.
    fn from_alloy(value: alloy::dyn_abi::DynSolValue) -> Result<Self, AbiDecodeError> {
        use alloy::dyn_abi::DynSolValue;

        match value {
            DynSolValue::Address(addr) => Ok(Self::Address(addr.into())),
            DynSolValue::Bool(b) => Ok(Self::Bool(b)),
            DynSolValue::Uint(u, _bits) => {
                // Convert U256 to BigUint
                let bytes = u.to_be_bytes_vec();
                Ok(Self::Uint(BigUint::from_bytes_be(&bytes)))
            }
            DynSolValue::Int(i, _bits) => {
                // Convert I256 to BigInt
                let (sign, abs) = i.into_sign_and_abs();
                let bytes = abs.to_be_bytes_vec();
                // Convert alloy Sign to num_bigint Sign
                let num_sign = match sign {
                    alloy::primitives::Sign::Positive => num_bigint::Sign::Plus,
                    alloy::primitives::Sign::Negative => num_bigint::Sign::Minus,
                };
                let big_int = BigInt::from_bytes_be(num_sign, &bytes);
                Ok(Self::Int(big_int))
            }
            DynSolValue::FixedBytes(fb, size) => {
                Ok(Self::FixedBytes(fb[..size].to_vec()))
            }
            DynSolValue::Bytes(b) => Ok(Self::Bytes(b)),
            DynSolValue::String(s) => Ok(Self::String(s)),
            DynSolValue::Array(arr) => {
                let values: Result<Vec<Self>, AbiDecodeError> =
                    arr.into_iter().map(Self::from_alloy).collect();
                Ok(Self::Array(values?))
            }
            DynSolValue::FixedArray(arr) => {
                let values: Result<Vec<Self>, AbiDecodeError> =
                    arr.into_iter().map(Self::from_alloy).collect();
                Ok(Self::Array(values?))
            }
            DynSolValue::Tuple(vals) => {
                // Treat tuples as arrays for Python compatibility
                let values: Result<Vec<Self>, AbiDecodeError> =
                    vals.into_iter().map(Self::from_alloy).collect();
                Ok(Self::Array(values?))
            }
            _ => Err(AbiDecodeError::UnsupportedType(format!(
                "Unsupported alloy value type"
            ))),
        }
    }

    /// Convert this value to a Python object.
    ///
    /// This is the single point where GIL is needed for conversion.
    fn to_python<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Self::Address(addr_bytes) => {
                let addr = Address::from_slice(addr_bytes);
                Ok(PyString::new(py, &addr.to_string()).into_any())
            }
            Self::Bool(b) => Ok(PyBool::new(py, *b).to_owned().into_any()),
            Self::FixedBytes(bytes) | Self::Bytes(bytes) => {
                Ok(PyBytes::new(py, bytes).into_any())
            }
            Self::Uint(n) => n.into_pyobject(py).map(pyo3::Bound::into_any),
            Self::Int(n) => n.into_pyobject(py).map(pyo3::Bound::into_any),
            Self::String(s) => Ok(PyString::new(py, s).into_any()),
            Self::Array(values) => {
                let list = PyList::empty(py);
                for value in values {
                    list.append(value.to_python(py)?)?;
                }
                Ok(list.into_any())
            }
        }
    }

    /// Convert this value to a Python object with raw hex address (no checksum).
    fn to_python_raw_address<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Self::Address(addr_bytes) => {
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
            _ => self.to_python(py),
        }
    }
}

// =============================================================================
// Pure Rust decoding functions
// =============================================================================

/// Decode ABI-encoded data for multiple types (pure Rust).
///
/// This is the core decoding function with no Python dependencies.
/// Use this for testing, parallel processing, or non-Python contexts.
///
/// # Arguments
///
/// * `types` - Slice of ABI type strings
/// * `data` - Raw ABI-encoded bytes
///
/// # Returns
///
/// A vector of `DecodedValue` enums representing the decoded values.
///
/// # Errors
///
/// Returns `AbiDecodeError` if decoding fails.
pub fn decode_rust(types: &[&str], data: &[u8]) -> Result<Vec<DecodedValue>, AbiDecodeError> {
    if types.is_empty() {
        return Err(AbiDecodeError::EmptyTypesList);
    }

    // Check for fixed-point types (not yet implemented)
    for ty in types {
        if ty.contains("fixed") || ty.contains("ufixed") {
            return Err(AbiDecodeError::UnsupportedType(
                "Fixed-point types (fixed/ufixed) are not yet implemented".to_string(),
            ));
        }
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    // Parse all types using DynSolType
    let mut parsed_types = Vec::with_capacity(types.len());
    for ty in types {
        let parsed = DynSolType::parse(ty).map_err(|e| {
            AbiDecodeError::UnsupportedType(format!("Invalid type '{ty}': {e}"))
        })?;
        parsed_types.push(parsed);
    }

    // Create a tuple type to decode all values at once
    let tuple_type = DynSolType::Tuple(parsed_types);

    // Decode the data
    let decoded = tuple_type
        .abi_decode(data)
        .map_err(|e| AbiDecodeError::InvalidOffset(format!("Decoding failed: {e}")))?;

    // Extract values from the tuple
    let values = match decoded {
        alloy::dyn_abi::DynSolValue::Tuple(vals) => vals,
        other => {
            // Single value case - wrap in a vec
            vec![other]
        }
    };

    // Convert each DynSolValue to DecodedValue
    values
        .into_iter()
        .map(DecodedValue::from_alloy)
        .collect()
}

/// Decode a single ABI value (pure Rust).
///
/// Convenience function for decoding a single value without Python dependencies.
pub fn decode_single_rust(abi_type: &str, data: &[u8]) -> Result<DecodedValue, AbiDecodeError> {
    if abi_type.contains("fixed") || abi_type.contains("ufixed") {
        return Err(AbiDecodeError::UnsupportedType(
            "Fixed-point types (fixed/ufixed) are not yet implemented".to_string(),
        ));
    }

    if data.is_empty() {
        return Err(AbiDecodeError::EmptyData);
    }

    let parsed = DynSolType::parse(abi_type).map_err(|e| {
        AbiDecodeError::UnsupportedType(format!("Invalid type '{abi_type}': {e}"))
    })?;

    let decoded = parsed
        .abi_decode(data)
        .map_err(|e| AbiDecodeError::InvalidOffset(format!("Decoding failed: {e}")))?;

    DecodedValue::from_alloy(decoded)
}

// =============================================================================
// Python conversion
// =============================================================================

/// Convert a slice of `DecodedValue` to a Python list.
fn decoded_values_to_py_list<'py>(
    py: Python<'py>,
    values: &[DecodedValue],
    checksum: bool,
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for value in values {
        let py_value = if checksum {
            value.to_python(py)?
        } else {
            value.to_python_raw_address(py)?
        };
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
#[allow(clippy::needless_pass_by_value)]
pub fn decode(
    py: Python<'_>,
    types: Vec<String>,
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    let type_refs: Vec<&str> = types.iter().map(String::as_str).collect();

    let values = py.detach(|| decode_rust(&type_refs, data)).map_err(|e| {
        if matches!(&e, AbiDecodeError::UnsupportedType(msg) if msg.contains("fixed")) {
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
            if matches!(&e, AbiDecodeError::UnsupportedType(msg) if msg.contains("fixed")) {
                PyNotImplementedError::new_err(format!("{e}"))
            } else {
                PyValueError::new_err(format!("{e}"))
            }
        })?;

    let py_value = if checksum {
        value.to_python(py)?
    } else {
        value.to_python_raw_address(py)?
    };
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

    #[test]
    fn test_decode_uint256_rust() {
        let mut data = vec![0u8; 32];
        data[30] = 0x30;
        data[31] = 0x39;

        let result = decode_single_rust("uint256", &data).expect("should decode uint256");
        match result {
            DecodedValue::Uint(n) => assert_eq!(n, BigUint::from(12345u64)),
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
            DecodedValue::Address(addr) => {
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
            DecodedValue::Bool(b) => assert!(b),
            _ => panic!("Expected Bool variant"),
        }

        data[31] = 0;
        let result = decode_single_rust("bool", &data).expect("should decode bool false");
        match result {
            DecodedValue::Bool(b) => assert!(!b),
            _ => panic!("Expected Bool variant"),
        }
    }

    #[test]
    fn test_decode_fixed_bytes_rust() {
        let data: Vec<u8> = (0..32).collect();

        let result = decode_single_rust("bytes32", &data).expect("should decode bytes32");
        match result {
            DecodedValue::FixedBytes(bytes) => {
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
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0,
        ]);

        let result = decode_single_rust("bytes", &data).expect("should decode bytes");
        match result {
            DecodedValue::Bytes(bytes) => {
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
            DecodedValue::String(s) => {
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
            DecodedValue::Array(values) => {
                assert_eq!(values.len(), 3);
                for (i, val) in values.iter().enumerate() {
                    match val {
                        DecodedValue::Uint(n) => assert_eq!(*n, BigUint::from(i as u64 + 1)),
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
            DecodedValue::Array(values) => {
                assert_eq!(values.len(), 2);
                match &values[0] {
                    DecodedValue::Uint(n) => assert_eq!(*n, BigUint::from(10u64)),
                    _ => panic!("Expected Uint"),
                }
                match &values[1] {
                    DecodedValue::Uint(n) => assert_eq!(*n, BigUint::from(20u64)),
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
            DecodedValue::Uint(n) => assert_eq!(*n, BigUint::from(42u64)),
            _ => panic!("Expected Uint"),
        }
        match &result[1] {
            DecodedValue::Bool(b) => assert!(*b),
            _ => panic!("Expected Bool"),
        }
        match &result[2] {
            DecodedValue::Address(addr) => {
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
            DecodedValue::Int(n) => assert_eq!(n, BigInt::from(-1)),
            _ => panic!("Expected Int variant"),
        }
    }

    #[test]
    fn test_decoded_value_to_python_roundtrip() {
        #[allow(unsafe_code)]
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                let val = DecodedValue::Uint(BigUint::from(123_456_789_u64));
                let py_val = val.to_python(py).expect("should convert to Python");
                let n: u64 = py_val.extract().expect("should extract as u64");
                assert_eq!(n, 123_456_789_u64);

                let val = DecodedValue::Bool(true);
                let py_val = val.to_python(py).expect("should convert to Python");
                let b: bool = py_val.extract().expect("should extract as bool");
                assert!(b);

                let val = DecodedValue::String("Hello".to_string());
                let py_val = val.to_python(py).expect("should convert to Python");
                let s: String = py_val.extract().expect("should extract as String");
                assert_eq!(s, "Hello");

                let val = DecodedValue::Array(vec![
                    DecodedValue::Uint(BigUint::from(1u64)),
                    DecodedValue::Uint(BigUint::from(2u64)),
                ]);
                let py_val = val.to_python(py).expect("should convert to Python");
                let list: Vec<u64> = py_val.extract().expect("should extract as Vec<u64>");
                assert_eq!(list, vec![1, 2]);
            });
        }
    }
}
