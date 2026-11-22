//! Cached Python function references for fast repeated conversions.
//!
//! `PyO3`'s `py.import()` has non-trivial overhead even when the module is cached
//! in `sys.modules`. This module stores function references in `PyOnceLock`
//! for one-time initialization and subsequent fast access.
//!
//! # GIL Safety
//!
//! `Python::attach()` calls in this module run on Tokio worker threads after
//! async RPC calls complete. These may block briefly while the Python event
//! loop or another Python thread holds the GIL. This cannot deadlock — see
//! `rpc::async_provider.rs` module-level comment for the full reasoning.

use pyo3::sync::PyOnceLock;
use pyo3::types::{PyAnyMethods, PyBytes, PyDictMethods};
use pyo3::{Bound, Py, PyAny, PyResult, Python};

/// Cached Python `int.from_bytes` function.
static INT_FROM_BYTES: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

/// Get the cached `int.from_bytes` function, initializing if needed.
fn get_int_from_bytes(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    INT_FROM_BYTES
        .get_or_try_init(py, || {
            let builtins = py.import("builtins")?;
            let int_class = builtins.getattr("int")?;
            let from_bytes = int_class.getattr("from_bytes")?;
            Ok(from_bytes.unbind())
        })
        .map(|cached| cached.bind(py))
}

/// Convert bytes to a Python `int` using cached `int.from_bytes`.
///
/// Uses tuple arguments for vectorcall protocol (faster than kwargs dict).
///
/// # Errors
///
/// Returns `PyErr` if the conversion fails.
pub fn bytes_to_int<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let from_bytes = get_int_from_bytes(py)?;
    let py_bytes = PyBytes::new(py, bytes);

    from_bytes.call1((py_bytes, "big"))
}

/// Convert bytes to a Python `int` (signed) using cached `int.from_bytes`.
///
/// Note: Uses kwargs because `signed` is a keyword-only parameter in Python.
///
/// # Errors
///
/// Returns `PyErr` if the conversion fails.
pub fn bytes_to_int_signed<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let from_bytes = get_int_from_bytes(py)?;
    let py_bytes = PyBytes::new(py, bytes);

    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("signed", true)?;

    from_bytes.call((py_bytes, "big"), Some(&kwargs))
}

/// Convert raw bytes to a plain Python `bytes` object.
///
/// The boundary crosses byte data as plain `bytes` — no hex-wrapper type.
///
/// # Errors
///
/// Never fails under normal use; the `PyResult` boundary keeps the helper
/// uniform with the other cache conversions.
pub fn to_py_bytes<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let py_bytes = PyBytes::new(py, bytes);
    Ok(py_bytes.into_any())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_bytes_to_int() {
        pyo3::Python::attach(|py| {
            let zero_bytes = [0u8; 32];
            let py_zero = bytes_to_int(py, &zero_bytes).unwrap();
            let val: u64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            let mut small_bytes = [0u8; 32];
            small_bytes[31] = 42;
            let py_small = bytes_to_int(py, &small_bytes).unwrap();
            let val: u64 = py_small.extract().unwrap();
            assert_eq!(val, 42);

            let max_bytes = [0xFFu8; 32];
            let py_max = bytes_to_int(py, &max_bytes).unwrap();
            let is_positive: bool = py_max
                .call_method1("__gt__", (0,))
                .unwrap()
                .extract()
                .unwrap();
            assert!(is_positive);
        });
    }

    #[test]
    fn test_bytes_to_int_signed() {
        pyo3::Python::attach(|py| {
            let mut pos_bytes = [0u8; 32];
            pos_bytes[31] = 42;
            let py_pos = bytes_to_int_signed(py, &pos_bytes).unwrap();
            let val: i64 = py_pos.extract().unwrap();
            assert_eq!(val, 42);

            let neg_bytes = [0xFFu8; 32];
            let py_neg = bytes_to_int_signed(py, &neg_bytes).unwrap();
            let val: i64 = py_neg.extract().unwrap();
            assert_eq!(val, -1);
        });
    }

    #[test]
    fn test_to_py_bytes() {
        pyo3::Python::attach(|py| {
            let empty_hb = to_py_bytes(py, &[]).unwrap();
            let empty_vec: Vec<u8> = empty_hb.extract().unwrap();
            assert!(empty_vec.is_empty());

            let test_bytes = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
            let hb = to_py_bytes(py, &test_bytes).unwrap();
            let roundtrip: Vec<u8> = hb.extract().unwrap();
            assert_eq!(roundtrip, test_bytes);

            let is_bytes: bool = hb.is_instance_of::<PyBytes>();
            assert!(is_bytes);
        });
    }

    #[test]
    fn test_caching_works() {
        pyo3::Python::attach(|py| {
            let _first = bytes_to_int(py, &[0u8; 32]).unwrap();

            let cached_ref = INT_FROM_BYTES.get(py).unwrap();

            let from_bytes = get_int_from_bytes(py).unwrap();
            assert!(from_bytes.is(cached_ref.bind(py)));
        });
    }
}
