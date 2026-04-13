//! `PyO3` conversions for Alloy primitive types.
//!
//! This module provides direct Python conversions for Alloy's `U256` and `I256`
//! types without intermediate `num-bigint` allocations.
//!
//! # Architecture
//!
//! The newtype wrapper pattern is used to work around Rust's orphan rules,
//! which prevent implementing foreign traits (`IntoPyObject` from `pyo3`)
//! for foreign types (`U256`/`I256` from `alloy`).

use crate::py_cache::{bytes_to_int, bytes_to_int_signed};
use alloy::primitives::{I256, U256};
use pyo3::prelude::*;
use pyo3::types::PyAny;

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
