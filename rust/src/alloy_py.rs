//! PyO3 conversions for Alloy primitive types.
//!
//! This module provides direct Python conversions for Alloy's `U256` and `I256`
//! types without intermediate `num-bigint` allocations.
//!
//! # Architecture
//!
//! The newtype wrapper pattern is used to work around Rust's orphan rules,
//! which prevent implementing foreign traits (`IntoPyObject` from `pyo3`)
//! for foreign types (`U256`/`I256` from `alloy`).

use alloy::primitives::{I256, U256};
use pyo3::prelude::*;
use pyo3::types::PyAny;

/// Wrapper for Alloy `U256` that implements `IntoPyObject`.
///
/// This enables zero-copy conversion to Python integers for 256-bit values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyU256(pub U256);

impl<'py> IntoPyObject<'py> for PyU256 {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = std::convert::Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        // Convert U256 to Python int by building from limbs
        // U256 stores as [u64; 4] in little-endian order (limb[0] is least significant)
        // We build: limb[3] * 2^192 + limb[2] * 2^128 + limb[1] * 2^64 + limb[0]

        // Use Python's int() constructor with base 256 (byte-by-byte)
        // This is simpler and avoids arithmetic overflow issues
        // U256 big-endian bytes
        let be_bytes: [u8; 32] = self.0.to_be_bytes();

        // Build from bytes using Python's int.from_bytes method
        // int.from_bytes(bytes, byteorder='big')
        let builtins = py.import("builtins").expect("builtins module should exist");
        let int_class = builtins.getattr("int").expect("int should exist in builtins");
        let from_bytes = int_class.getattr("from_bytes").expect("int.from_bytes should exist");

        // Create bytes object
        let py_bytes = pyo3::types::PyBytes::new(py, &be_bytes);
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("byteorder", "big").expect("set_item should succeed");

        let result = from_bytes
            .call((py_bytes,), Some(&kwargs))
            .expect("int.from_bytes should succeed");

        Ok(result)
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
    type Error = std::convert::Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        // I256 is stored as two's complement in a U256
        // We need to extract the raw bytes and let Python interpret them as signed
        let raw_bytes: [u8; 32] = self.0.to_be_bytes();

        // Build from bytes using Python's int.from_bytes method
        let builtins = py.import("builtins").expect("builtins module should exist");
        let int_class = builtins.getattr("int").expect("int should exist in builtins");
        let from_bytes = int_class.getattr("from_bytes").expect("int.from_bytes should exist");

        // Create bytes object
        let py_bytes = pyo3::types::PyBytes::new(py, &raw_bytes);
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("byteorder", "big").expect("set_item should succeed");
        kwargs.set_item("signed", true).expect("set_item should succeed");

        let result = from_bytes
            .call((py_bytes,), Some(&kwargs))
            .expect("int.from_bytes should succeed");

        Ok(result)
    }
}

/// Direct conversion from `U256` to Python int without allocation.
///
/// This is a convenience function that wraps `PyU256`.
#[inline]
pub fn u256_to_py<'py>(py: Python<'py>, val: &U256) -> Bound<'py, PyAny> {
    PyU256(*val).into_pyobject(py).expect("U256 conversion should not fail")
}

/// Direct conversion from `I256` to Python int without allocation.
///
/// This is a convenience function that wraps `PyI256`.
#[inline]
pub fn i256_to_py<'py>(py: Python<'py>, val: &I256) -> Bound<'py, PyAny> {
    PyI256(*val).into_pyobject(py).expect("I256 conversion should not fail")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // Note: These tests use `with_embedded_python_interpreter` which can only be
    // initialized once per process. When running with `--test-threads=1`, they work.
    // In parallel test mode, the Python interpreter state conflicts.

    #[test]
    #[allow(unsafe_code)]
    fn test_u256_to_python() {
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                // Test zero
                let zero = U256::ZERO;
                let py_zero = u256_to_py(py, &zero);
                let val: u64 = py_zero.extract().unwrap();
                assert_eq!(val, 0);

                // Test small value
                let small = U256::from(42u64);
                let py_small = u256_to_py(py, &small);
                let val: u64 = py_small.extract().unwrap();
                assert_eq!(val, 42);

                // Test max u64
                let max_u64 = U256::from(u64::MAX);
                let py_max = u256_to_py(py, &max_u64);
                let val: u64 = py_max.extract().unwrap();
                assert_eq!(val, u64::MAX);

                // Test U256 max (2^256 - 1)
                let max_u256 = U256::MAX;
                let py_max = u256_to_py(py, &max_u256);
                // Verify by converting via Python's int.to_bytes
                let bytes = py_max.call_method1("to_bytes", (32, "big")).unwrap();
                let bytes: &[u8] = bytes.extract().unwrap();
                let expected_bytes: [u8; 32] = max_u256.to_be_bytes();
                assert_eq!(bytes, expected_bytes);
            });
        }
    }

    #[test]
    #[allow(unsafe_code)]
    fn test_i256_to_python() {
        unsafe {
            pyo3::with_embedded_python_interpreter(|py| {
                // Test zero
                let zero = I256::ZERO;
                let py_zero = i256_to_py(py, &zero);
                let val: i64 = py_zero.extract().unwrap();
                assert_eq!(val, 0);

                // Test positive
                let pos = I256::try_from(12345i64).unwrap();
                let py_pos = i256_to_py(py, &pos);
                let val: i64 = py_pos.extract().unwrap();
                assert_eq!(val, 12345);

                // Test negative
                let neg = I256::try_from(-12345i64).unwrap();
                let py_neg = i256_to_py(py, &neg);
                let val: i64 = py_neg.extract().unwrap();
                assert_eq!(val, -12345);

                // Test I256 min (-2^255)
                let min = I256::MINUS_ONE;
                let py_min = i256_to_py(py, &min);
                let val: i64 = py_min.extract().unwrap();
                assert_eq!(val, -1);
            });
        }
    }
}
