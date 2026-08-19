//! Cached Python module references for zero-overhead conversions.
//!
//! `PyO3`'s `py.import()` has non-trivial overhead even when the module is cached
//! in `sys.modules`. This module stores function references in `PyOnceLock`
//! for one-time initialization and subsequent fast access.
//!
//! # Performance
//!
//! - First call: Full module import + attribute lookup
//! - Subsequent calls: Single `PyOnceLock` lookup + `Bind::bind()` (no Python import machinery)
//!
//! # Thread Safety
//!
//! All cached items use `PyOnceLock` which is GIL-aware and thread-safe.
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
///
/// Used for U256/I256 to Python int conversion without intermediate allocations.
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

    // Use tuple arguments for vectorcall: (bytes, "big")
    // This is faster than creating a kwargs dict
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

    // `byteorder` is positional, `signed` is keyword-only
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("signed", true)?;

    from_bytes.call((py_bytes, "big"), Some(&kwargs))
}

/// Convert raw bytes to a plain Python `bytes` object.
///
/// The `HexBytes` wrapper type was abolished (ergo JWXZ4A, option D): the
/// boundary container crosses FFI as plain `bytes`, and the `py.import`
/// class-lookup machinery this helper used to need is gone.
pub fn to_py_bytes<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    let py_bytes = PyBytes::new(py, bytes);
    Ok(py_bytes.into_any())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::print_stderr)]

    use super::*;

    #[test]
    fn test_bytes_to_int() {
        pyo3::Python::attach(|py| {
            // Test zero
            let zero_bytes = [0u8; 32];
            let py_zero = bytes_to_int(py, &zero_bytes).unwrap();
            let val: u64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            // Test small value
            let mut small_bytes = [0u8; 32];
            small_bytes[31] = 42;
            let py_small = bytes_to_int(py, &small_bytes).unwrap();
            let val: u64 = py_small.extract().unwrap();
            assert_eq!(val, 42);

            // Test max U256 (all 0xFF bytes)
            let max_bytes = [0xFFu8; 32];
            let py_max = bytes_to_int(py, &max_bytes).unwrap();
            // Verify it's a large positive number
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
            // Test positive
            let mut pos_bytes = [0u8; 32];
            pos_bytes[31] = 42;
            let py_pos = bytes_to_int_signed(py, &pos_bytes).unwrap();
            let val: i64 = py_pos.extract().unwrap();
            assert_eq!(val, 42);

            // Test negative (-1 in two's complement)
            let neg_bytes = [0xFFu8; 32];
            let py_neg = bytes_to_int_signed(py, &neg_bytes).unwrap();
            let val: i64 = py_neg.extract().unwrap();
            assert_eq!(val, -1);
        });
    }

    #[test]
    fn test_to_py_bytes() {
        pyo3::Python::attach(|py| {
            // Plain bytes: `.hex()` is the bare (unprefixed) hex string.
            let empty_hb = to_py_bytes(py, &[]).unwrap();
            let hex_str: String = empty_hb.call_method0("hex").unwrap().extract().unwrap();
            assert_eq!(hex_str, "");

            let test_bytes = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
            let hb = to_py_bytes(py, &test_bytes).unwrap();
            let hex_str: String = hb.call_method0("hex").unwrap().extract().unwrap();
            assert_eq!(hex_str, "0123456789abcdef");

            // It is a plain `bytes` instance (not a subclass, not str).
            let is_bytes: bool = hb
                .call_method1("isinstance", (py.eval("bytes").unwrap(),))
                .unwrap()
                .extract()
                .unwrap();
            assert!(is_bytes);
        });
    }

    #[test]
    fn test_caching_works() {
        pyo3::Python::attach(|py| {
            // First call initializes cache
            let _first = bytes_to_int(py, &[0u8; 32]).unwrap();

            // Get the cached reference
            let cached_ref = INT_FROM_BYTES.get(py).unwrap();

            // Second call should use same cached object
            let from_bytes = get_int_from_bytes(py).unwrap();
            assert!(from_bytes.is(cached_ref.bind(py)));
        });
    }
}
