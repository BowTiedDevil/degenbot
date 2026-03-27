//! ABI decoding for Ethereum data.
//!
//! High-performance decoding of ABI-encoded data.

use alloy_primitives::Address;
use num_bigint::{BigInt, BigUint};
use pyo3::{
    exceptions::{PyNotImplementedError, PyValueError},
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};
use std::borrow::Cow;
use thiserror::Error;

/// Size of a word in ABI encoding (32 bytes).
const WORD_SIZE: usize = 32;

/// Size of an Ethereum address in bytes.
const ADDRESS_BYTES: usize = 20;

/// Offset of address data within a word (32 - 20 = 12).
const ADDRESS_OFFSET_IN_WORD: usize = WORD_SIZE - ADDRESS_BYTES;

/// Represents the different ABI types.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum AbiType {
    Address,
    Bool,
    Bytes(usize), // 0 for dynamic, N for fixed-size bytesN
    Uint(usize),  // bits (8-256) - stored for validation
    Int(usize),   // bits (8-256) - stored for validation
    String,
}

/// Represents array information for a type.
#[derive(Clone, Debug)]
enum ArrayKind {
    Fixed(usize), // Known size
    Dynamic,      // Dynamic array (e.g., uint256[])
    None,         // Not an array
}

/// Parsed type information to avoid repeated parsing.
/// For array types, stores the unparsed base type string to handle nested arrays.
#[derive(Clone, Debug)]
struct ParsedType {
    base: AbiType,
    array: ArrayKind,
    /// For array types, the unparsed base type string (may contain nested arrays)
    base_str: Option<String>,
    /// Cached dynamic status to avoid re-parsing
    is_dynamic: bool,
}

impl ParsedType {
    fn new(ty: &str) -> Result<Self, AbiDecodeError> {
        let (base_cow, array) = parse_type_and_array(ty)?;

        if matches!(array, ArrayKind::None) {
            // Not an array - parse the base type directly
            let base = parse_base_type(&base_cow)?;
            let is_dynamic = matches!(base, AbiType::Bytes(0) | AbiType::String);
            Ok(Self {
                base,
                array,
                base_str: None,
                is_dynamic,
            })
        } else {
            // It's an array - store the base as a string for potential nested arrays
            // Convert Cow to owned String
            let base_str: String = match base_cow {
                Cow::Borrowed(s) => s.to_string(),
                Cow::Owned(s) => s,
            };

            // Try to parse the base type; if it fails, the inner type might be a nested array
            let base = parse_base_type(&base_str).unwrap_or_else(|_| {
                // If base type parsing fails, try to determine the inner type's dynamic status
                // by parsing it as a ParsedType
                Self::new(&base_str)
                    .map(|p| p.base)
                    .unwrap_or(AbiType::Bytes(0))
            });

            // Calculate dynamic status once during construction
            let is_dynamic = match array {
                ArrayKind::Dynamic => true,
                ArrayKind::Fixed(_) => Self::new(&base_str).map(|p| p.is_dynamic).unwrap_or(false),
                ArrayKind::None => unreachable!(),
            };

            Ok(Self {
                base,
                array,
                base_str: Some(base_str),
                is_dynamic,
            })
        }
    }

    #[inline]
    #[allow(dead_code)]
    const fn is_dynamic(&self) -> bool {
        self.is_dynamic
    }

    fn element_type_str(&self) -> Option<&str> {
        self.base_str.as_deref()
    }
}

/// Error type for ABI decoding operations.
#[derive(Debug, Clone, Error)]
enum AbiDecodeError {
    #[error("Invalid array size in type: {0}")]
    InvalidArraySize(String),
    #[error("Insufficient data: need {needed} bytes at offset {offset}, have {have} bytes")]
    InsufficientData {
        needed: usize,
        have: usize,
        offset: usize,
    },
    #[error("Invalid offset: {0}")]
    InvalidOffset(String),
    #[error("Invalid length: {0}")]
    InvalidLength(String),
    #[error("Unsupported type: {0}")]
    UnsupportedType(String),
}

impl From<AbiDecodeError> for PyErr {
    fn from(err: AbiDecodeError) -> Self {
        PyValueError::new_err(err.to_string())
    }
}

/// Normalize a type string by applying aliases.
#[inline]
fn normalize_type(ty: &str) -> &str {
    match ty.trim() {
        "uint" => "uint256",
        "int" => "int256",
        "function" => "bytes24",
        other => other,
    }
}

/// Parse the base type string into an `AbiType`.
///
/// Note: This function assumes the type has already been normalized.
#[inline]
fn parse_base_type(normalized: &str) -> Result<AbiType, AbiDecodeError> {
    match normalized {
        "address" => Ok(AbiType::Address),
        "bool" => Ok(AbiType::Bool),
        "bytes" => Ok(AbiType::Bytes(0)), // Dynamic bytes
        "string" => Ok(AbiType::String),
        // Fast path for common fixed bytes sizes
        "bytes1" => Ok(AbiType::Bytes(1)),
        "bytes2" => Ok(AbiType::Bytes(2)),
        "bytes4" => Ok(AbiType::Bytes(4)),
        "bytes8" => Ok(AbiType::Bytes(8)),
        "bytes16" => Ok(AbiType::Bytes(16)),
        "bytes20" => Ok(AbiType::Bytes(20)),
        "bytes24" => Ok(AbiType::Bytes(24)),
        "bytes32" => Ok(AbiType::Bytes(32)),
        // Fast path for common uint sizes
        "uint8" => Ok(AbiType::Uint(8)),
        "uint16" => Ok(AbiType::Uint(16)),
        "uint32" => Ok(AbiType::Uint(32)),
        "uint64" => Ok(AbiType::Uint(64)),
        "uint128" => Ok(AbiType::Uint(128)),
        "uint256" => Ok(AbiType::Uint(256)),
        // Fast path for common int sizes
        "int8" => Ok(AbiType::Int(8)),
        "int16" => Ok(AbiType::Int(16)),
        "int32" => Ok(AbiType::Int(32)),
        "int64" => Ok(AbiType::Int(64)),
        "int128" => Ok(AbiType::Int(128)),
        "int256" => Ok(AbiType::Int(256)),
        // Generic parsing for other sizes
        t => {
            if let Some(n_str) = t.strip_prefix("bytes") {
                let n = n_str
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0 && n <= WORD_SIZE)
                    .ok_or_else(|| AbiDecodeError::UnsupportedType(t.to_string()))?;
                Ok(AbiType::Bytes(n))
            } else if let Some(n_str) = t.strip_prefix("uint") {
                let bits = n_str
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0 && n <= 256 && n % 8 == 0)
                    .ok_or_else(|| AbiDecodeError::UnsupportedType(t.to_string()))?;
                Ok(AbiType::Uint(bits))
            } else if let Some(n_str) = t.strip_prefix("int") {
                let bits = n_str
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0 && n <= 256 && n % 8 == 0)
                    .ok_or_else(|| AbiDecodeError::UnsupportedType(t.to_string()))?;
                Ok(AbiType::Int(bits))
            } else {
                Err(AbiDecodeError::UnsupportedType(t.to_string()))
            }
        }
    }
}

/// Parse a type string and extract the base type and array info.
///
/// Returns the normalized base type string and the array kind.
/// Uses `Cow` to avoid allocations for already-normalized types.
#[inline]
fn parse_type_and_array(ty: &str) -> Result<(Cow<'_, str>, ArrayKind), AbiDecodeError> {
    let trimmed = ty.trim();

    // Check if it's an array type - use bytes for faster parsing
    if let Some(bracket_pos) = trimmed.as_bytes().iter().rposition(|&b| b == b'[') {
        let base = &trimmed[..bracket_pos];
        let array_part = &trimmed[bracket_pos..];

        if array_part == "[]" {
            // Dynamic array - normalize the base
            return Ok((
                Cow::Owned(normalize_type(base).to_string()),
                ArrayKind::Dynamic,
            ));
        } else if array_part.len() > 2
            && array_part.as_bytes()[0] == b'['
            && array_part.as_bytes()[array_part.len() - 1] == b']'
        {
            // Fixed-size array
            let size_str = &array_part[1..array_part.len() - 1];
            match size_str.parse::<usize>() {
                Ok(size) if size > 0 => {
                    return Ok((
                        Cow::Owned(normalize_type(base).to_string()),
                        ArrayKind::Fixed(size),
                    ))
                }
                _ => return Err(AbiDecodeError::InvalidArraySize(ty.to_string())),
            }
        }
    }

    // Not an array - just normalize
    let normalized = normalize_type(trimmed);
    // If normalized is different from trimmed, it's an alias and we need to allocate
    if normalized.len() == trimmed.len() {
        // Common case: already normalized, no allocation needed
        Ok((Cow::Borrowed(normalized), ArrayKind::None))
    } else {
        Ok((Cow::Owned(normalized.to_string()), ArrayKind::None))
    }
}

/// Convert bytes to a Python int (`BigUint`).
#[inline]
fn bytes_to_uint(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

/// Convert bytes to a Python int (`BigInt`, signed).
#[inline]
fn bytes_to_int(bytes: &[u8]) -> BigInt {
    BigInt::from_signed_bytes_be(bytes)
}

/// Read an offset and length from data at the given position.
/// Returns (offset, length) or an error if the data is invalid.
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

    Ok((content_offset + WORD_SIZE, length))
}

/// Decode a single static value from data at the given offset.
/// Returns the decoded value and the number of bytes consumed (always 32 for static types).
fn decode_static_value(
    py: Python<'_>,
    ty: &AbiType,
    data: &[u8],
    offset: usize,
    checksum: bool,
) -> PyResult<(Py<PyAny>, usize)> {
    if offset + WORD_SIZE > data.len() {
        return Err(PyValueError::new_err(format!(
            "Insufficient data: need {} bytes at offset {}, have {} bytes",
            WORD_SIZE,
            offset,
            data.len()
        )));
    }

    let word = &data[offset..offset + WORD_SIZE];

    let value: Py<PyAny> = match ty {
        AbiType::Address => {
            let addr_bytes = &word[ADDRESS_OFFSET_IN_WORD..WORD_SIZE];
            let value = if checksum {
                // Use the standard checksummed format from alloy
                let addr = Address::from_slice(addr_bytes);
                PyString::new(py, &addr.to_string())
            } else {
                // Optimize: use stack buffer to avoid heap allocation
                // Format: "0x" + 40 hex chars = 42 bytes
                let mut buf = [0u8; 42];
                buf[0] = b'0';
                buf[1] = b'x';
                // SAFETY: hex::encode_to_slice writes exactly 2*len bytes
                let _ = hex::encode_to_slice(addr_bytes, &mut buf[2..]);
                // SAFETY: buf contains only ASCII characters
                let addr_str = unsafe { std::str::from_utf8_unchecked(&buf) };
                PyString::new(py, addr_str)
            };
            value.into()
        }
        AbiType::Bool => {
            let is_true = word[WORD_SIZE - 1] != 0;
            PyBool::new(py, is_true).to_owned().into()
        }
        AbiType::Bytes(n) => {
            let start = WORD_SIZE - *n;
            PyBytes::new(py, &word[start..WORD_SIZE]).into()
        }
        AbiType::Uint(_) => bytes_to_uint(word).into_pyobject(py)?.into(),
        AbiType::Int(_) => bytes_to_int(word).into_pyobject(py)?.into(),
        AbiType::String => {
            return Err(PyValueError::new_err(
                "String type should be decoded as dynamic value",
            ));
        }
    };

    Ok((value, WORD_SIZE))
}

/// Decode a dynamic type (bytes or string) from data.
/// Returns the decoded value and the number of bytes consumed from the head.
fn decode_dynamic_value(
    py: Python<'_>,
    ty: &AbiType,
    data: &[u8],
    read_offset: usize,
) -> PyResult<(Py<PyAny>, usize)> {
    let (data_start, length) = read_offset_and_length(data, read_offset)
        .map_err(|e| PyValueError::new_err(format!("Error reading dynamic type: {e}")))?;

    let data_end = data_start
        .checked_add(length)
        .ok_or_else(|| PyValueError::new_err("Integer overflow calculating data end"))?;

    if data_end > data.len() {
        return Err(PyValueError::new_err("Dynamic data extends beyond buffer"));
    }

    let dynamic_data = &data[data_start..data_end];

    let value: Py<PyAny> = match ty {
        AbiType::Bytes(0) => PyBytes::new(py, dynamic_data).into(),
        AbiType::String => {
            let s = std::str::from_utf8(dynamic_data)
                .map_err(|e| PyValueError::new_err(format!("Invalid UTF-8 in string: {e}")))?;
            PyString::new(py, s).into()
        }
        _ => {
            return Err(PyValueError::new_err(format!(
                "Type {ty:?} cannot be decoded as dynamic value"
            )));
        }
    };

    Ok((value, WORD_SIZE)) // Head is always 32 bytes for dynamic types
}

/// Decode an array from data.
fn decode_array(
    py: Python<'_>,
    parsed: &ParsedType,
    data: &[u8],
    read_offset: usize,
    checksum: bool,
) -> PyResult<(Py<PyAny>, usize)> {
    // For arrays, we need the element type string to handle nested arrays
    let inner_type_str = parsed
        .element_type_str()
        .ok_or_else(|| PyValueError::new_err("Expected array type but got non-array type"))?;

    // Parse the inner type (this handles nested arrays recursively)
    let inner_parsed = ParsedType::new(inner_type_str).map_err(PyErr::from)?;

    let (length, data_start) = match parsed.array {
        ArrayKind::Fixed(size) => (size, read_offset),
        ArrayKind::Dynamic => {
            let (content_start, length) = read_offset_and_length(data, read_offset)
                .map_err(|e| PyValueError::new_err(format!("Error reading array: {e}")))?;
            (length, content_start)
        }
        ArrayKind::None => {
            return Err(PyValueError::new_err("Expected array type"));
        }
    };

    let mut current_offset = data_start;
    let mut values = Vec::with_capacity(length);

    for _ in 0..length {
        let (value, consumed) = decode_value(py, &inner_parsed, data, current_offset, checksum)?;
        values.push(value);
        current_offset += consumed;
    }

    let list = PyList::new(py, values)?;

    let head_size = match parsed.array {
        ArrayKind::Fixed(_) => current_offset - read_offset,
        ArrayKind::Dynamic => WORD_SIZE,
        ArrayKind::None => unreachable!(),
    };

    Ok((list.into(), head_size))
}

/// Decode a value of a specific base type (non-array).
fn decode_value_of_type(
    py: Python<'_>,
    ty: &AbiType,
    data: &[u8],
    offset: usize,
    checksum: bool,
) -> PyResult<(Py<PyAny>, usize)> {
    match ty {
        AbiType::Bytes(0) | AbiType::String => decode_dynamic_value(py, ty, data, offset),
        _ => decode_static_value(py, ty, data, offset, checksum),
    }
}

/// Decode a value from a parsed type.
fn decode_value(
    py: Python<'_>,
    parsed: &ParsedType,
    data: &[u8],
    offset: usize,
    checksum: bool,
) -> PyResult<(Py<PyAny>, usize)> {
    match parsed.array {
        ArrayKind::Fixed(_) | ArrayKind::Dynamic => {
            decode_array(py, parsed, data, offset, checksum)
        }
        ArrayKind::None => decode_value_of_type(py, &parsed.base, data, offset, checksum),
    }
}

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
/// Internal implementation of decode that works with `&[&str]`.
///
/// This separates the core logic from the `PyO3` interface, allowing:
/// - Better testability (no `PyO3` required for unit tests)
/// - Cleaner Rust API for internal use
/// - Easier reuse in non-Python contexts
fn decode_impl(
    py: Python<'_>,
    types: &[&str],
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    if types.is_empty() {
        return Err(PyValueError::new_err("Types list cannot be empty"));
    }

    // Check for fixed-point types (not yet implemented)
    for ty in types {
        if ty.contains("fixed") || ty.contains("ufixed") {
            return Err(PyNotImplementedError::new_err(
                "Fixed-point types (fixed/ufixed) are not yet implemented",
            ));
        }
    }

    if data.is_empty() {
        return Err(PyValueError::new_err("Data cannot be empty"));
    }

    // Parse all types once and cache the results
    let mut parsed_types = Vec::with_capacity(types.len());
    for ty in types {
        parsed_types.push(ParsedType::new(ty).map_err(PyErr::from)?);
    }

    let mut values = Vec::with_capacity(types.len());
    let mut offset = 0;

    for parsed in &parsed_types {
        let (value, consumed) = decode_value(py, parsed, data, offset, checksum)?;
        values.push(value);
        offset += consumed;
    }

    let list = PyList::new(py, values)?;
    Ok(list.into())
}

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
/// This `PyO3`-exposed function is a thin wrapper around `decode_impl`. `PyO3` requires
/// owned types (`Vec<String>`) for Python-to-Rust list conversions, so we convert
/// `Vec<String>` to `Vec<&str>` (cheap pointer copies) before calling the internal
/// implementation. This separation enables:
/// - Clean internal APIs using `&[&str]`
/// - Unit testing without `PyO3` dependencies
/// - Reuse in non-Python Rust code
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
    // Convert Vec<String> to Vec<&str> - cheap operation, just pointer copies
    let type_refs: Vec<&str> = types.iter().map(String::as_str).collect();
    decode_impl(py, &type_refs, data, strict, checksum)
}

/// Internal implementation of `decode_single`.
///
/// Separates core logic from `PyO3` interface for testability and reuse.
fn decode_single_impl(
    py: Python<'_>,
    ty: &str,
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    // Check for fixed-point types (not yet implemented)
    if ty.contains("fixed") || ty.contains("ufixed") {
        return Err(PyNotImplementedError::new_err(
            "Fixed-point types (fixed/ufixed) are not yet implemented",
        ));
    }

    if data.is_empty() {
        return Err(PyValueError::new_err("Data cannot be empty"));
    }

    let parsed = ParsedType::new(ty).map_err(PyErr::from)?;
    let (value, _) = decode_value(py, &parsed, data, 0, checksum)?;
    Ok(value)
}

/// Decode a single ABI value.
///
/// Convenience function for decoding a single value.
///
/// This PyO3-exposed function wraps `decode_single_impl` for consistent architecture.
#[pyfunction]
#[pyo3(signature = (ty, data, strict = true, checksum = true))]
pub fn decode_single(
    py: Python<'_>,
    ty: &str,
    data: &[u8],
    strict: bool,
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    decode_single_impl(py, ty, data, strict, checksum)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_normalize_type() {
        assert_eq!(normalize_type("uint"), "uint256");
        assert_eq!(normalize_type("int"), "int256");
        assert_eq!(normalize_type("function"), "bytes24");
        assert_eq!(normalize_type("uint256"), "uint256");
        assert_eq!(normalize_type("address"), "address");
    }

    #[test]
    fn test_parse_base_type() {
        assert!(matches!(parse_base_type("address"), Ok(AbiType::Address)));
        assert!(matches!(parse_base_type("bool"), Ok(AbiType::Bool)));
        assert!(matches!(parse_base_type("uint256"), Ok(AbiType::Uint(256))));
        assert!(matches!(parse_base_type("int128"), Ok(AbiType::Int(128))));
        assert!(matches!(parse_base_type("bytes32"), Ok(AbiType::Bytes(32))));
        assert!(matches!(parse_base_type("bytes"), Ok(AbiType::Bytes(0))));
        assert!(matches!(parse_base_type("string"), Ok(AbiType::String)));
    }

    #[test]
    fn test_parse_type_and_array() {
        let (base, arr) = parse_type_and_array("uint256").unwrap();
        assert_eq!(base, "uint256");
        assert!(matches!(arr, ArrayKind::None));

        let (base, arr) = parse_type_and_array("uint256[]").unwrap();
        assert_eq!(base, "uint256");
        assert!(matches!(arr, ArrayKind::Dynamic));

        let (base, arr) = parse_type_and_array("uint256[3]").unwrap();
        assert_eq!(base, "uint256");
        assert!(matches!(arr, ArrayKind::Fixed(3)));
    }

    #[test]
    fn test_parse_type_invalid_array_size() {
        let result = parse_type_and_array("uint256[invalid]");
        assert!(matches!(result, Err(AbiDecodeError::InvalidArraySize(_))));
    }

    #[test]
    fn test_parse_type_with_aliases() {
        let parsed = ParsedType::new("uint").unwrap();
        assert!(matches!(parsed.base, AbiType::Uint(256)));
        assert!(matches!(parsed.array, ArrayKind::None));

        let parsed = ParsedType::new("int").unwrap();
        assert!(matches!(parsed.base, AbiType::Int(256)));

        let parsed = ParsedType::new("function").unwrap();
        assert!(matches!(parsed.base, AbiType::Bytes(24)));
    }

    #[test]
    fn test_parsed_type_is_dynamic() {
        // Static types
        assert!(!ParsedType::new("uint256").unwrap().is_dynamic());
        assert!(!ParsedType::new("address").unwrap().is_dynamic());
        assert!(!ParsedType::new("bool").unwrap().is_dynamic());
        assert!(!ParsedType::new("bytes32").unwrap().is_dynamic());
        assert!(!ParsedType::new("uint256[3]").unwrap().is_dynamic());

        // Dynamic types
        assert!(ParsedType::new("bytes").unwrap().is_dynamic());
        assert!(ParsedType::new("string").unwrap().is_dynamic());
        assert!(ParsedType::new("uint256[]").unwrap().is_dynamic());
        assert!(ParsedType::new("address[][3]").unwrap().is_dynamic());
    }

    #[test]
    fn test_read_offset_and_length() {
        // Create test data: offset = 64, length = 5
        let mut data = vec![0u8; 128];
        // At position 0: offset value (64)
        data[31] = 64;
        // At position 64: length value (5)
        data[95] = 5;

        let (content_start, length) = read_offset_and_length(&data, 0).unwrap();
        assert_eq!(content_start, 64 + 32); // offset + WORD_SIZE
        assert_eq!(length, 5);
    }

    #[test]
    fn test_read_offset_and_length_insufficient_data() {
        let data = vec![0u8; 16];
        let result = read_offset_and_length(&data, 0);
        assert!(matches!(
            result,
            Err(AbiDecodeError::InsufficientData { .. })
        ));
    }
}
