//! ABI decoding for Ethereum data.
//!
//! High-performance decoding of ABI-encoded data.
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

use crate::abi_types::AbiType;
use crate::errors::AbiDecodeError;
use alloy::hex;
use alloy::primitives::Address;
use num_bigint::{BigInt, BigUint};
use pyo3::{
    exceptions::{PyNotImplementedError, PyValueError},
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};

/// Size of a word in ABI encoding (32 bytes).
const WORD_SIZE: usize = 32;

/// Size of an Ethereum address in bytes.
const ADDRESS_BYTES: usize = 20;

/// Offset of address data within a word (32 - 20 = 12).
const ADDRESS_OFFSET_IN_WORD: usize = WORD_SIZE - ADDRESS_BYTES;

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
    Address([u8; ADDRESS_BYTES]),
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
            Self::FixedBytes(bytes) | Self::Bytes(bytes) => Ok(PyBytes::new(py, bytes).into_any()),
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
// Byte utilities - Pure Rust
// =============================================================================

/// Convert bytes to a `BigUint`.
#[inline]
fn bytes_to_uint(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

/// Convert bytes to a `BigInt` (signed).
#[inline]
fn bytes_to_int(bytes: &[u8]) -> BigInt {
    BigInt::from_signed_bytes_be(bytes)
}

/// Read an offset and length from data at the given position.
/// Returns (`data_start`, length) or an error if the data is invalid.
#[inline]
fn read_offset_and_length(
    data: &[u8],
    read_offset: usize,
) -> Result<(usize, usize), AbiDecodeError> {
    if read_offset + WORD_SIZE > data.len() {
        return Err(AbiDecodeError::InsufficientData {
            needed: WORD_SIZE,
            have: data.len(),
            offset: read_offset,
        });
    }

    let offset_bytes = &data[read_offset..read_offset + WORD_SIZE];
    let content_offset: usize = bytes_to_uint(offset_bytes)
        .try_into()
        .map_err(|_| AbiDecodeError::InvalidOffset("value too large".to_string()))?;

    if content_offset + WORD_SIZE > data.len() {
        return Err(AbiDecodeError::InvalidOffset(format!(
            "offset {content_offset} points beyond data length {}",
            data.len()
        )));
    }

    let length_bytes = &data[content_offset..content_offset + WORD_SIZE];
    let length: usize = bytes_to_uint(length_bytes)
        .try_into()
        .map_err(|_| AbiDecodeError::InvalidLength("value too large".to_string()))?;

    let data_start = content_offset
        .checked_add(WORD_SIZE)
        .ok_or_else(|| AbiDecodeError::InvalidOffset("arithmetic overflow".to_string()))?;

    Ok((data_start, length))
}

// =============================================================================
// Pure Rust decoding functions
// =============================================================================

/// Decode a single static value from data at the given offset.
/// Returns the decoded value and the number of bytes consumed (always 32 for static types).
fn decode_static_value_rust(
    type_: &AbiType,
    data: &[u8],
    offset: usize,
) -> Result<(DecodedValue, usize), AbiDecodeError> {
    if offset + WORD_SIZE > data.len() {
        return Err(AbiDecodeError::InsufficientData {
            needed: WORD_SIZE,
            have: data.len(),
            offset,
        });
    }

    let word = &data[offset..offset + WORD_SIZE];

    let value: DecodedValue = match type_ {
        AbiType::Address => {
            let mut addr_bytes = [0u8; ADDRESS_BYTES];
            addr_bytes.copy_from_slice(&word[ADDRESS_OFFSET_IN_WORD..WORD_SIZE]);
            DecodedValue::Address(addr_bytes)
        }
        AbiType::Bool => {
            let is_true = word[WORD_SIZE - 1] != 0;
            DecodedValue::Bool(is_true)
        }
        AbiType::FixedBytes(n) => {
            let start = WORD_SIZE - *n;
            DecodedValue::FixedBytes(word[start..WORD_SIZE].to_vec())
        }
        AbiType::Uint(_) => DecodedValue::Uint(bytes_to_uint(word)),
        AbiType::Int(_) => DecodedValue::Int(bytes_to_int(word)),
        AbiType::Bytes | AbiType::String | AbiType::Array(_) | AbiType::FixedArray(_, _) => {
            return Err(AbiDecodeError::InvalidOffset(
                "Dynamic or array type should not be decoded as static value".to_string(),
            ));
        }
    };

    Ok((value, WORD_SIZE))
}

/// Decode a dynamic type (bytes or string) from data.
/// Returns the decoded value and the number of bytes consumed from the head.
fn decode_dynamic_value_rust(
    type_: &AbiType,
    data: &[u8],
    read_offset: usize,
) -> Result<(DecodedValue, usize), AbiDecodeError> {
    let (data_start, length) = read_offset_and_length(data, read_offset)?;

    let data_end = data_start
        .checked_add(length)
        .ok_or_else(|| AbiDecodeError::InvalidLength("arithmetic overflow".to_string()))?;

    if data_end > data.len() {
        return Err(AbiDecodeError::InsufficientData {
            needed: data_end,
            have: data.len(),
            offset: data_start,
        });
    }

    let dynamic_data = &data[data_start..data_end];

    let value: DecodedValue = match type_ {
        AbiType::Bytes => DecodedValue::Bytes(dynamic_data.to_vec()),
        AbiType::String => {
            let s = std::str::from_utf8(dynamic_data).map_err(|e| {
                AbiDecodeError::InvalidLength(format!("Invalid UTF-8 in string: {e}"))
            })?;
            DecodedValue::String(s.to_string())
        }
        _ => {
            return Err(AbiDecodeError::UnsupportedType(format!(
                "Type {type_:?} cannot be decoded as dynamic value"
            )));
        }
    };

    Ok((value, WORD_SIZE))
}

/// Decode an array from data.
fn decode_array_rust(
    element_type: &AbiType,
    length: usize,
    data: &[u8],
    data_start: usize,
) -> Result<(DecodedValue, usize), AbiDecodeError> {
    let mut current_offset = data_start;
    let mut values = Vec::with_capacity(length);

    for _ in 0..length {
        let (value, consumed) = decode_value_rust(element_type, data, current_offset)?;
        values.push(value);
        current_offset = current_offset.checked_add(consumed).ok_or_else(|| {
            AbiDecodeError::InvalidOffset("arithmetic overflow in offset calculation".to_string())
        })?;
    }

    let head_size = current_offset - data_start;
    Ok((DecodedValue::Array(values), head_size))
}

/// Decode a value from an `AbiType`.
fn decode_value_rust(
    type_: &AbiType,
    data: &[u8],
    offset: usize,
) -> Result<(DecodedValue, usize), AbiDecodeError> {
    match type_ {
        AbiType::Bytes | AbiType::String => decode_dynamic_value_rust(type_, data, offset),
        AbiType::Array(element_type) => {
            let (data_start, length) = read_offset_and_length(data, offset)?;
            decode_array_rust(element_type, length, data, data_start)
        }
        AbiType::FixedArray(element_type, size) => {
            let size = *size;
            decode_array_rust(element_type, size, data, offset)
        }
        _ => decode_static_value_rust(type_, data, offset),
    }
}

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

    // Parse all types once using the shared AbiType parser
    let mut parsed_types = Vec::with_capacity(types.len());
    for ty in types {
        let parsed =
            AbiType::parse(ty).map_err(|e| AbiDecodeError::UnsupportedType(e.to_string()))?;
        parsed_types.push(parsed);
    }

    let mut values = Vec::with_capacity(types.len());
    let mut offset = 0;

    for parsed in &parsed_types {
        let (value, consumed) = decode_value_rust(parsed, data, offset)?;
        values.push(value);
        offset += consumed;
    }

    Ok(values)
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

    let parsed =
        AbiType::parse(abi_type).map_err(|e| AbiDecodeError::UnsupportedType(e.to_string()))?;
    let (value, _) = decode_value_rust(&parsed, data, 0)?;
    Ok(value)
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
    fn test_parse_base_type() {
        assert!(matches!(AbiType::parse("address"), Ok(AbiType::Address)));
        assert!(matches!(AbiType::parse("bool"), Ok(AbiType::Bool)));
        assert!(matches!(AbiType::parse("uint256"), Ok(AbiType::Uint(256))));
        assert!(matches!(AbiType::parse("int128"), Ok(AbiType::Int(128))));
        assert!(matches!(
            AbiType::parse("bytes32"),
            Ok(AbiType::FixedBytes(32))
        ));
        assert!(matches!(AbiType::parse("bytes"), Ok(AbiType::Bytes)));
        assert!(matches!(AbiType::parse("string"), Ok(AbiType::String)));
    }

    #[test]
    fn test_parse_array_types() {
        assert!(matches!(AbiType::parse("uint256"), Ok(AbiType::Uint(256))));
        assert!(matches!(AbiType::parse("uint256[]"), Ok(AbiType::Array(_))));
        assert!(matches!(
            AbiType::parse("uint256[3]"),
            Ok(AbiType::FixedArray(_, 3))
        ));
    }

    #[test]
    fn test_parse_type_invalid_array_size() {
        let result = AbiType::parse("uint256[invalid]");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_type_returns_error() {
        let result = AbiType::parse("invalid_type");
        assert!(result.is_err());

        let result = AbiType::parse("invalid_type[]");
        assert!(result.is_err());

        let result = AbiType::parse("uint256");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_type_with_aliases() {
        assert!(matches!(AbiType::parse("uint"), Ok(AbiType::Uint(256))));
        assert!(matches!(AbiType::parse("int"), Ok(AbiType::Int(256))));
        assert!(matches!(
            AbiType::parse("function"),
            Ok(AbiType::FixedBytes(24))
        ));
    }

    #[test]
    fn test_is_dynamic() {
        assert!(!AbiType::parse("uint256").unwrap().is_dynamic());
        assert!(!AbiType::parse("address").unwrap().is_dynamic());
        assert!(!AbiType::parse("bool").unwrap().is_dynamic());
        assert!(!AbiType::parse("bytes32").unwrap().is_dynamic());
        assert!(!AbiType::parse("uint256[3]").unwrap().is_dynamic());

        assert!(AbiType::parse("bytes").unwrap().is_dynamic());
        assert!(AbiType::parse("string").unwrap().is_dynamic());
        assert!(AbiType::parse("uint256[]").unwrap().is_dynamic());
        assert!(AbiType::parse("address[][3]").unwrap().is_dynamic());
    }

    #[test]
    fn test_read_offset_and_length() {
        let mut data = vec![0u8; 128];
        data[31] = 64;
        data[95] = 5;

        let (content_start, length) =
            read_offset_and_length(&data, 0).expect("valid offset/length data should parse");
        assert_eq!(content_start, 64 + 32);
        assert_eq!(length, 5);
    }

    #[test]
    fn test_read_offset_and_length_insufficient_data() {
        let data = vec![0u8; 16];
        let result = read_offset_and_length(&data, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_static_value_boundary_conditions() {
        let data = vec![0u8; 32];
        let offset: usize = 0;
        let word_size: usize = 32;
        assert!(offset + word_size <= data.len());

        let data = vec![0u8; 31];
        let offset: usize = 0;
        assert!(offset + word_size > data.len());

        let data = vec![0u8; 32];
        let offset: usize = 1;
        assert!(offset + word_size > data.len());

        let data = vec![0u8; 33];
        let offset: usize = 1;
        assert!(offset + word_size <= data.len());

        let data = vec![0u8; 64];
        let offset: usize = 32;
        assert!(offset + word_size <= data.len());
    }

    #[test]
    fn test_offset_overflow_protection() {
        let result = usize::MAX.checked_add(1);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_offset_overflow_in_return() {
        let max_offset = usize::MAX - WORD_SIZE + 1;
        let result = max_offset.checked_add(WORD_SIZE);
        assert!(result.is_none());

        let safe_offset = usize::MAX - WORD_SIZE;
        let result = safe_offset.checked_add(WORD_SIZE);
        assert!(result.is_some());
    }

    // =========================================================================
    // Pure Rust decoding tests
    // =========================================================================

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
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
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
