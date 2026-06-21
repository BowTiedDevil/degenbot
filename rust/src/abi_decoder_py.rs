//! `PyO3` wrappers for ABI decoding.
//!
//! Thin binding layer over [`degenbot_abi::abi_decoder`] (the pure-Rust core).
//! The core `decode_rust` / `decode_single_rust` / `decode_for_types` functions
//! live in the `degenbot-abi` workspace member and have no `PyO3` dependency;
//! this module adds the Python-object conversion (`AbiValue` → Python) and the
//! `#[pyfunction]` entry points registered in [`crate::lib`]'s `#[pymodule]`.

use degenbot_abi::abi_decoder::{decode_rust, decode_single_rust};
use degenbot_abi::abi_types::AbiValue;
use degenbot_core::errors::AbiDecodeError;
use pyo3::{
    exceptions::{PyNotImplementedError, PyValueError},
    prelude::*,
    types::{PyBool, PyBytes, PyList, PyString},
};

use crate::conversion::alloy as alloy_py;

// =============================================================================
// Python conversion
// =============================================================================

/// Convert an `AbiValue` to a Python object.
fn abi_value_to_python<'py>(
    value: &AbiValue,
    py: Python<'py>,
    checksum: bool,
) -> PyResult<Bound<'py, PyAny>> {
    match value {
        AbiValue::Address(addr_bytes) => {
            if checksum {
                let addr = alloy::primitives::Address::from_slice(addr_bytes);
                Ok(PyString::new(py, &addr.to_string()).into_any())
            } else {
                let s = alloy::hex::encode_prefixed(addr_bytes);
                Ok(PyString::new(py, &s).into_any())
            }
        }
        AbiValue::Bool(b) => Ok(PyBool::new(py, *b).to_owned().into_any()),
        AbiValue::FixedBytes(bytes) | AbiValue::Bytes(bytes) => {
            Ok(PyBytes::new(py, bytes).into_any())
        }
        AbiValue::Uint(n, _) => alloy_py::u256_to_py(py, n),
        AbiValue::Int(n, _) => alloy_py::i256_to_py(py, n),
        AbiValue::String(s) => Ok(PyString::new(py, s).into_any()),
        AbiValue::Array(values) => {
            let py_values: Vec<_> = values
                .iter()
                .map(|v| abi_value_to_python(v, py, checksum))
                .collect::<Result<_, _>>()?;
            Ok(PyList::new(py, py_values)?.into_any())
        }
    }
}

/// Convert a slice of `AbiValue` to a Python list.
fn decoded_values_to_py_list<'py>(
    py: Python<'py>,
    values: &[AbiValue],
    checksum: bool,
) -> PyResult<Bound<'py, PyList>> {
    let py_values: Vec<_> = values
        .iter()
        .map(|v| abi_value_to_python(v, py, checksum))
        .collect::<Result<_, _>>()?;
    PyList::new(py, py_values)
}

// =============================================================================
// Error mapping
// =============================================================================

/// Map `AbiDecodeError` to the appropriate Python exception.
///
/// Fixed-point types map to `NotImplementedError`; all others map to `ValueError`.
fn map_decode_error(e: &AbiDecodeError) -> PyErr {
    if matches!(e, AbiDecodeError::FixedPointNotImplemented) {
        PyNotImplementedError::new_err(e.to_string())
    } else {
        PyValueError::new_err(e.to_string())
    }
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
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
#[pyo3(signature = (types, data, checksum = true))]
pub fn decode(
    py: Python<'_>,
    types: &Bound<'_, PyList>,
    data: &[u8],
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    // Extract type strings from Python list (borrow directly, no String alloc)
    let type_strings: Vec<String> = types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;
    let type_refs: Vec<&str> = type_strings.iter().map(String::as_str).collect();

    let values = py
        .detach(|| decode_rust(&type_refs, data))
        .map_err(|e| map_decode_error(&e))?;

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
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
#[pyo3(signature = (abi_type, data, checksum = true))]
pub fn decode_single(
    py: Python<'_>,
    abi_type: &str,
    data: &[u8],
    checksum: bool,
) -> PyResult<Py<PyAny>> {
    let value = py
        .detach(|| decode_single_rust(abi_type, data))
        .map_err(|e| map_decode_error(&e))?;

    let py_value = abi_value_to_python(&value, py, checksum)?;
    Ok(py_value.unbind())
}

// =============================================================================
// Python-touching tests (moved from the pre-split abi_decoder.rs)
// =============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use alloy::primitives::U256;
    use degenbot_abi::abi_types::AbiValue;

    #[test]
    fn test_abi_value_to_python_roundtrip() {
        pyo3::Python::attach(|py| {
            let val = AbiValue::Uint(U256::from(123_456_789_u64), 256);
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
                AbiValue::Uint(U256::from(1u64), 256),
                AbiValue::Uint(U256::from(2u64), 256),
            ]);
            let py_val = abi_value_to_python(&val, py, true).expect("should convert to Python");
            let list: Vec<u64> = py_val.extract().expect("should extract as Vec<u64>");
            assert_eq!(list, vec![1, 2]);
        });
    }
}
