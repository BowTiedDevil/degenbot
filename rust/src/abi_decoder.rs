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

/// Type aliases for ABI types.
const TYPE_ALIASES: &[(&str, &str)] = &[
    ("uint", "uint256"),
    ("int", "int256"),
    ("function", "bytes24"),
];

/// Normalize a type string by applying aliases.
fn normalize_type(ty: &str) -> String {
    let normalized = ty.trim();
    for (alias, canonical) in TYPE_ALIASES {
        if normalized == *alias {
            return canonical.to_string();
        }
    }
    normalized.to_string()
}

/// Parse a type string and extract the base type and array info.
fn parse_type(ty: &str) -> PyResult<(String, Option<usize>)> {
    let normalized = normalize_type(ty);

    // Check if it's an array type
    if let Some(bracket_pos) = normalized.rfind('[') {
        let base = &normalized[..bracket_pos];
        let array_part = &normalized[bracket_pos..];

        if array_part == "[]" {
            // Dynamic array - return the full type with [] suffix so we can detect it
            return Ok((normalized, None));
        } else if array_part.starts_with('[') && array_part.ends_with(']') {
            // Fixed-size array
            let size_str = &array_part[1..array_part.len() - 1];
            match size_str.parse::<usize>() {
                Ok(size) => return Ok((base.to_string(), Some(size))),
                Err(_) => return Err(PyValueError::new_err(format!("Invalid array size in type: {ty}"))),
            }
        }
    }

    // Not an array
    Ok((normalized, None))
}

/// Convert bytes to a Python int (`BigUint`).
fn bytes_to_uint(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

/// Convert bytes to a Python int (`BigInt`, signed).
fn bytes_to_int(bytes: &[u8]) -> BigInt {
    BigInt::from_signed_bytes_be(bytes)
}

/// Decode a single static value from data at the given offset.
/// Returns the decoded value and the number of bytes consumed (always 32 for static types).
fn decode_static_value(
    py: Python<'_>,
    ty: &str,
    data: &[u8],
    offset: usize,
) -> PyResult<(Py<PyAny>, usize)> {
    if offset + 32 > data.len() {
        return Err(PyValueError::new_err(format!(
            "Insufficient data: need 32 bytes at offset {}, have {} bytes",
            offset,
            data.len()
        )));
    }

    let word = &data[offset..offset + 32];

    let value: Py<PyAny> = match ty {
        "address" => {
            // Address is the last 20 bytes of the 32-byte word
            let addr_bytes = &word[12..32];
            let addr = Address::from_slice(addr_bytes);
            PyString::new(py, &addr.to_string()).into()
        }
        "bool" => {
            let is_true = word[31] != 0;
            PyBool::new(py, is_true).to_owned().into()
        }
        "bytes1" => PyBytes::new(py, &word[31..32]).into(),
        "bytes2" => PyBytes::new(py, &word[30..32]).into(),
        "bytes3" => PyBytes::new(py, &word[29..32]).into(),
        "bytes4" => PyBytes::new(py, &word[28..32]).into(),
        "bytes8" => PyBytes::new(py, &word[24..32]).into(),
        "bytes16" => PyBytes::new(py, &word[16..32]).into(),
        "bytes20" => PyBytes::new(py, &word[12..32]).into(),
        "bytes24" => PyBytes::new(py, &word[8..32]).into(),
        "bytes32" => PyBytes::new(py, word).into(),
        t if t.starts_with("uint") => {
            // Unsigned integer - use num-bigint feature which provides IntoPyObject
            bytes_to_uint(word).into_pyobject(py)?.into()
        }
        t if t.starts_with("int") => {
            // Signed integer - use num-bigint feature which provides IntoPyObject
            bytes_to_int(word).into_pyobject(py)?.into()
        }
        _ => {
            return Err(PyValueError::new_err(format!("Unsupported type: {ty}")));
        }
    };

    Ok((value, 32))
}

/// Decode a dynamic type (bytes or string) from data.
/// Returns the decoded value and the number of bytes consumed from the head.
fn decode_dynamic_value(
    py: Python<'_>,
    ty: &str,
    data: &[u8],
    head_offset: usize,
) -> PyResult<(Py<PyAny>, usize)> {
    if head_offset + 32 > data.len() {
        return Err(PyValueError::new_err("Insufficient data for dynamic type offset"));
    }

    // Read the offset to the data
    let offset_bytes = &data[head_offset..head_offset + 32];
    let data_offset = bytes_to_uint(offset_bytes)
        .try_into()
        .map_err(|_| PyValueError::new_err("Invalid offset for dynamic type"))?;

    if data_offset + 32 > data.len() {
        return Err(PyValueError::new_err("Offset points beyond data"));
    }

    // Read the length
    let length_bytes = &data[data_offset..data_offset + 32];
    let length: usize = bytes_to_uint(length_bytes)
        .try_into()
        .map_err(|_| PyValueError::new_err("Invalid length for dynamic type"))?;

    let data_start = data_offset + 32;
    let data_end = data_start + length;

    if data_end > data.len() {
        return Err(PyValueError::new_err("Dynamic data extends beyond buffer"));
    }

    let dynamic_data = &data[data_start..data_end];

    let value: Py<PyAny> = match ty {
        "bytes" => PyBytes::new(py, dynamic_data).into(),
        "string" => {
            let s = String::from_utf8(dynamic_data.to_vec())
                .map_err(|e| PyValueError::new_err(format!("Invalid UTF-8 in string: {e}")))?;
            PyString::new(py, &s).into()
        }
        _ => {
            return Err(PyValueError::new_err(format!(
                "Unknown dynamic type: {ty}"
            )));
        }
    };

    Ok((value, 32)) // Head is always 32 bytes for dynamic types
}

/// Decode an array from data.
fn decode_array(
    py: Python<'_>,
    base_type: &str,
    fixed_size: Option<usize>,
    data: &[u8],
    head_offset: usize,
) -> PyResult<(Py<PyAny>, usize)> {
    let (length, data_start) = if let Some(size) = fixed_size {
        // Fixed-size array - length is known
        (size, head_offset)
    } else {
        // Dynamic array - read offset and length
        if head_offset + 32 > data.len() {
            return Err(PyValueError::new_err("Insufficient data for array offset"));
        }

        let offset_bytes = &data[head_offset..head_offset + 32];
        let array_offset: usize = bytes_to_uint(offset_bytes)
            .try_into()
            .map_err(|_| PyValueError::new_err("Invalid array offset"))?;

        if array_offset + 32 > data.len() {
            return Err(PyValueError::new_err("Array offset points beyond data"));
        }

        let length_bytes = &data[array_offset..array_offset + 32];
        let length: usize = bytes_to_uint(length_bytes)
            .try_into()
            .map_err(|_| PyValueError::new_err("Invalid array length"))?;

        (length, array_offset + 32)
    };

    let list = PyList::empty(py);
    let mut current_offset = data_start;

    for _ in 0..length {
        let (value, consumed) = decode_value(py, base_type, data, current_offset)?;
        list.append(value)?;
        current_offset += consumed;
    }

    let head_size = if fixed_size.is_some() { current_offset - head_offset } else { 32 };
    Ok((list.into(), head_size))
}

/// Decode a value of any type.
/// Returns the decoded value and the number of bytes consumed.
fn decode_value(
    py: Python<'_>,
    ty: &str,
    data: &[u8],
    offset: usize,
) -> PyResult<(Py<PyAny>, usize)> {
    let (base_type, array_size) = parse_type(ty)?;

    if let Some(size) = array_size {
        // It's an array
        return decode_array(py, &base_type, Some(size), data, offset);
    }

    if base_type.ends_with("[]") {
        // Dynamic array - remove the [] suffix
        let inner_type = &base_type[..base_type.len() - 2];
        return decode_array(py, inner_type, None, data, offset);
    }

    if base_type == "bytes" || base_type == "string" {
        // Dynamic types
        return decode_dynamic_value(py, &base_type, data, offset);
    }

    // Static types
    decode_static_value(py, &base_type, data, offset)
}

/// Decode ABI-encoded data for multiple types.
///
/// # Arguments
///
/// * `types` - List of ABI type strings
/// * `data` - Raw ABI-encoded bytes
/// * `strict` - If true (default), performs strict validation
///
/// # Returns
///
/// A list of decoded Python values.
#[pyfunction]
#[pyo3(signature = (types, data, strict = true))]
#[allow(clippy::needless_pass_by_value)]
pub fn decode(py: Python<'_>, types: Vec<String>, data: &[u8], strict: bool) -> PyResult<Py<PyAny>> {
    if !strict {
        return Err(PyNotImplementedError::new_err(
            "Non-strict decoding mode is not yet implemented",
        ));
    }

    if types.is_empty() {
        return Err(PyValueError::new_err("Types list cannot be empty"));
    }

    // Check for fixed-point types (not yet implemented)
    for ty in &types {
        if ty.contains("fixed") || ty.contains("ufixed") {
            return Err(PyNotImplementedError::new_err(
                "Fixed-point types (fixed/ufixed) are not yet implemented",
            ));
        }
    }

    if data.is_empty() {
        return Err(PyValueError::new_err("Data cannot be empty"));
    }

    let list = PyList::empty(py);
    let mut offset = 0;

    for ty in &types {
        let (value, consumed) = decode_value(py, ty, data, offset)?;
        list.append(value)?;
        offset += consumed;
    }

    Ok(list.into())
}

/// Decode a single ABI value.
///
/// Convenience function for decoding a single value.
#[pyfunction]
#[pyo3(signature = (ty, data, strict = true))]
pub fn decode_single(py: Python<'_>, ty: &str, data: &[u8], strict: bool) -> PyResult<Py<PyAny>> {
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

    let (value, _) = decode_value(py, ty, data, 0)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
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
    fn test_parse_type() {
        let (base, arr) = parse_type("uint256").expect("uint256 should parse");
        assert_eq!(base, "uint256");
        assert_eq!(arr, None);

        let (base, arr) = parse_type("uint256[]").expect("uint256[] should parse");
        assert_eq!(base, "uint256[]");
        assert_eq!(arr, None);

        let (base, arr) = parse_type("uint256[3]").expect("uint256[3] should parse");
        assert_eq!(base, "uint256");
        assert_eq!(arr, Some(3));
    }
}
