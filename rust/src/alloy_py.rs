//! `PyO3` conversions for Alloy primitive types.
//!
//! This module provides direct Python conversions for Alloy's `U256` and `I256`
//! types without intermediate `num-bigint` allocations.
//!
//! It also provides `abi_value_from_python` for converting arbitrary Python
//! objects into `AbiValue` enums for ABI encoding.
//!
//! # Architecture
//!
//! The newtype wrapper pattern is used to work around Rust's orphan rules,
//! which prevent implementing foreign traits (`IntoPyObject` from `pyo3`)
//! for foreign types (`U256`/`I256` from `alloy`).

use crate::abi_types::AbiValue;
use crate::py_cache::{bytes_to_int, bytes_to_int_signed};
use alloy::primitives::{I256, U256};
use pyo3::{exceptions::PyValueError, prelude::*};
use pyo3::types::{PyAny, PyBool, PyBytes, PyDict, PyList, PyString};
use std::str::FromStr;

/// Wrapper for Alloy `U256` that implements `IntoPyObject`.
///
/// This enables conversion to Python integers without intermediate `num-bigint` allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyU256(pub U256);

impl<'py> IntoPyObject<'py> for PyU256 {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        bytes_to_int(py, &self.0.to_be_bytes::<32>())
    }
}

/// Wrapper for Alloy `I256` that implements `IntoPyObject`.
///
/// Handles signed 256-bit integers with proper two's complement conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyI256(pub I256);

impl<'py> IntoPyObject<'py> for PyI256 {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        bytes_to_int_signed(py, &self.0.to_be_bytes::<32>())
    }
}

/// Direct conversion from `U256` to Python int without intermediate `num-bigint` allocation.
///
/// This is a convenience function that wraps `PyU256`.
///
/// # Errors
///
/// Returns an error if Python's `int.from_bytes` fails (extremely unlikely for valid U256).
pub fn u256_to_py<'py>(py: Python<'py>, val: &U256) -> PyResult<Bound<'py, PyAny>> {
    PyU256(*val).into_pyobject(py)
}

/// Direct conversion from `I256` to Python int without intermediate `num-bigint` allocation.
///
/// This is a convenience function that wraps `PyI256`.
///
/// # Errors
///
/// Returns an error if Python's `int.from_bytes` fails (extremely unlikely for valid I256).
pub fn i256_to_py<'py>(py: Python<'py>, val: &I256) -> PyResult<Bound<'py, PyAny>> {
    PyI256(*val).into_pyobject(py)
}

// =============================================================================
// Python → U256 extraction
// =============================================================================

/// Extract a `U256` from a Python object.
///
/// Accepts:
/// - Small integers (via `i128` extraction)
/// - Large integers (via Python's `to_bytes`)
/// - Raw bytes slices
///
/// # Errors
///
/// Returns `PyValueError` if the object cannot be converted to `U256`,
/// or if the integer is negative, or if the value exceeds 256 bits.
pub(crate) fn extract_python_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    // Try small integer first (covers most cases efficiently)
    if let Ok(int_val) = obj.extract::<i128>() {
        if int_val < 0 {
            return Err(PyValueError::new_err("Value cannot be negative"));
        }
        return Ok(U256::from(int_val.cast_unsigned()));
    }

    // For larger integers, convert via Python's to_bytes
    let int_type = obj.py().import("builtins")?.getattr("int")?;
    if obj.is_instance(&int_type)? {
        let kwargs = PyDict::new(obj.py());
        kwargs.set_item("signed", false)?;
        let bytes = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes.extract()?;
        return U256::try_from_be_slice(bytes).ok_or_else(|| {
            PyValueError::new_err("Value is too large (exceeds 256 bits)")
        });
    }

    // Try raw bytes
    if let Ok(bytes) = obj.extract::<&[u8]>() {
        return U256::try_from_be_slice(bytes)
            .ok_or_else(|| PyValueError::new_err("Failed to parse value from bytes"));
    }

    Err(PyValueError::new_err(
        "Value must be an integer or bytes",
    ))
}

// =============================================================================
// Python → AbiValue conversion
// =============================================================================

/// Create an `AbiValue` from a Python object.
///
/// Converts Python types to their ABI equivalents:
/// - `bool` → `AbiValue::Bool`
/// - `int` → `AbiValue::Uint` or `AbiValue::Int`
/// - `str` ("0x..." with 42 chars) → `AbiValue::Address`
/// - `str` (other) → `AbiValue::String`
/// - `bytes` → `AbiValue::Bytes`
/// - `list` → `AbiValue::Array`
///
/// # Errors
///
/// Returns `PyValueError` if the Python object cannot be converted.
pub fn abi_value_from_python(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<AbiValue> {
    // Recursive helper that captures py from outer scope
    fn convert_item(item: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<AbiValue> {
        abi_value_from_python(py, item)
    }

    // Try bool first (before int, since bool is subclass of int in Python)
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(AbiValue::Bool(b.is_true()));
    }

    // Try int - extract as i128 first to detect sign, then convert to U256/I256
    // Python integers can be arbitrarily large, but we only support up to 256 bits
    if let Ok(int_val) = obj.extract::<i128>() {
        // Small integer fits in i128
        if int_val >= 0 {
            return Ok(AbiValue::Uint(U256::from(int_val.cast_unsigned())));
        }
        return Ok(AbiValue::Int(I256::try_from(int_val).map_err(|_| {
            PyValueError::new_err("Integer conversion failed")
        })?));
    }

    // For larger integers, try to extract via Python's to_bytes method
    // Check if it's an integer type
    let int_type = py.import("builtins")?.getattr("int")?;
    if obj.is_instance(&int_type)? {
        // Try to get the sign
        let is_negative: bool = obj.call_method1("__lt__", (0,))?.extract()?;

        if is_negative {
            // Negative integer - use signed to_bytes for correct I256::MIN handling
            // signed=True handles two's complement encoding, including I256::MIN
            let bytes = obj.call_method1("to_bytes", (32, "big", true))?;
            let bytes: &[u8] = bytes.extract()?;
            let u256 = U256::from_be_bytes(
                <[u8; 32]>::try_from(bytes).map_err(|_| {
                    PyValueError::new_err("Integer value out of range for int256")
                })?,
            );
            // Directly interpret as I256 (two's complement encoding)
            return Ok(AbiValue::Int(I256::from_raw(u256)));
        }
        // Positive integer - use unsigned to_bytes
        let bytes = obj.call_method1("to_bytes", (32, "big", false))?;
        let bytes: &[u8] = bytes.extract()?;
        let u256 = U256::from_be_bytes(
            <[u8; 32]>::try_from(bytes).map_err(|_| {
                PyValueError::new_err("Integer value out of range for uint256")
            })?,
        );
        return Ok(AbiValue::Uint(u256));
    }

    // Try string (for addresses)
    if let Ok(s) = obj.cast::<PyString>() {
        let s = s.to_string();
        // Check if it's an address
        if s.starts_with("0x") && s.len() == 42 {
            let addr = alloy::primitives::Address::from_str(&s)
                .map_err(|e| PyValueError::new_err(format!("Invalid address '{s}': {e}")))?;
            return Ok(AbiValue::Address(addr.into()));
        }
        return Ok(AbiValue::String(s));
    }

    // Try bytes
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(AbiValue::Bytes(b.as_bytes().to_vec()));
    }

    // Try list (for arrays)
    if let Ok(list) = obj.cast::<PyList>() {
        let values: Result<Vec<_>, _> =
            list.iter().map(|item| convert_item(&item, py)).collect();
        return Ok(AbiValue::Array(values?));
    }

    Err(PyValueError::new_err(format!(
        "Cannot convert Python object to ABI value: {}",
        obj.repr()?
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_u256_to_python() {
        pyo3::Python::attach(|py| {
            // Test zero
            let zero = U256::ZERO;
            let py_zero = u256_to_py(py, &zero).unwrap();
            let val: u64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            // Test small value
            let small = U256::from(42u64);
            let py_small = u256_to_py(py, &small).unwrap();
            let val: u64 = py_small.extract().unwrap();
            assert_eq!(val, 42);

            // Test max u64
            let max_u64 = U256::from(u64::MAX);
            let py_max = u256_to_py(py, &max_u64).unwrap();
            let val: u64 = py_max.extract().unwrap();
            assert_eq!(val, u64::MAX);

            // Test U256 max (2^256 - 1)
            let max_u256 = U256::MAX;
            let py_max = u256_to_py(py, &max_u256).unwrap();
            // Verify by converting via Python's int.to_bytes
            let bytes = py_max.call_method1("to_bytes", (32, "big")).unwrap();
            let bytes: &[u8] = bytes.extract().unwrap();
            let expected_bytes: [u8; 32] = max_u256.to_be_bytes();
            assert_eq!(bytes, expected_bytes);
        });
    }

    #[test]
    fn test_extract_python_u256() {
        pyo3::Python::attach(|py| {
            // Test small integer
            let small = pyo3::types::PyInt::new(py, 42);
            let val = extract_python_u256(&small).unwrap();
            assert_eq!(val, U256::from(42u64));

            // Test zero
            let zero = pyo3::types::PyInt::new(py, 0);
            let val = extract_python_u256(&zero).unwrap();
            assert_eq!(val, U256::ZERO);

            // Test negative → error
            let neg = pyo3::types::PyInt::new(py, -1);
            assert!(extract_python_u256(&neg).is_err());

            // Test large integer (u64 max)
            let large = pyo3::types::PyInt::new(py, i128::from(u64::MAX));
            let val = extract_python_u256(&large).unwrap();
            assert_eq!(val, U256::from(u64::MAX));

            // Test bytes input
            let bytes = pyo3::types::PyBytes::new(py, &[0u8; 32]);
            let val = extract_python_u256(&bytes).unwrap();
            assert_eq!(val, U256::ZERO);

            // Test bytes input with value
            let mut data = [0u8; 32];
            data[31] = 0xff;
            let bytes = pyo3::types::PyBytes::new(py, &data);
            let val = extract_python_u256(&bytes).unwrap();
            assert_eq!(val, U256::from(255u64));
        });
    }

    #[test]
    fn test_i256_to_python() {
        pyo3::Python::attach(|py| {
            // Test zero
            let zero = I256::ZERO;
            let py_zero = i256_to_py(py, &zero).unwrap();
            let val: i64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            // Test positive
            let pos = I256::try_from(12345i64).unwrap();
            let py_pos = i256_to_py(py, &pos).unwrap();
            let val: i64 = py_pos.extract().unwrap();
            assert_eq!(val, 12345);

            // Test negative
            let neg = I256::try_from(-12345i64).unwrap();
            let py_neg = i256_to_py(py, &neg).unwrap();
            let val: i64 = py_neg.extract().unwrap();
            assert_eq!(val, -12345);

            // Test I256 -1 (MINUS_ONE)
            let min = I256::MINUS_ONE;
            let py_min = i256_to_py(py, &min).unwrap();
            let val: i64 = py_min.extract().unwrap();
            assert_eq!(val, -1);
        });
    }
}
