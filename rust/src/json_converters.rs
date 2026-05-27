//! Python to JSON conversion utilities.
//!
//! Provides functions to convert Python objects to `serde_json::Value` for
//! JSON-RPC requests. Used by both sync and async provider implementations.
//!
//! # GIL Safety
//!
//! `Python::attach()` calls in this module run on Tokio worker threads after
//! async RPC calls complete. These may block briefly while the Python event
//! loop or another Python thread holds the GIL. This cannot deadlock — see
//! `async_provider.rs` module-level comment for the full reasoning.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyList};

/// Convert a Python list to a JSON array.
#[allow(clippy::missing_errors_doc)]
pub fn python_list_to_json(list: &Bound<'_, PyList>) -> PyResult<serde_json::Value> {
    let mut arr = Vec::with_capacity(list.len());
    for item in list.iter() {
        let json_val = python_to_json(&item)?;
        arr.push(json_val);
    }
    Ok(serde_json::Value::Array(arr))
}

/// Convert a Python object to a JSON value.
///
/// Handles the following Python types:
/// - `None` → JSON `null`
/// - `bool` → JSON `bool`
/// - `int` → JSON `number` (or string for large integers)
/// - `float` → JSON `number` (or `null` for NaN/Inf)
/// - `str` → JSON `string`
/// - `bytes` → JSON `string` (hex-encoded with "0x" prefix)
/// - `list` → JSON `array`
/// - `dict` → JSON `object`
/// - Other → JSON `string` (via `str()`, with warning)
#[allow(clippy::missing_errors_doc)]
pub fn python_to_json(obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    // None -> null
    if obj.is_none() {
        return Ok(serde_json::Value::Null);
    }

    // bool (check before int since bool is subclass of int in Python)
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(serde_json::Value::Bool(b.is_true()));
    }

    // int - try i64 first (covers most cases efficiently)
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(serde_json::Value::Number(i.into()));
    }

    // Large int - only use str() for values that don't fit i64
    if let Ok(int_type) = obj.py().import("builtins")?.getattr("int") {
        if obj.is_instance(&int_type)? {
            // Get string representation for large integers
            if let Ok(s) = obj.str() {
                let s = s.to_string();
                // Try to fit in i64 again (might be a large negative)
                if let Ok(n) = s.parse::<i64>() {
                    return Ok(serde_json::Value::Number(n.into()));
                }
                // For larger numbers, return as string (JSON doesn't support arbitrary precision)
                return Ok(serde_json::Value::String(s));
            }
        }
    }

    // float
    if let Ok(f) = obj.extract::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Ok(serde_json::Value::Number(n));
        }
        return Ok(serde_json::Value::Null); // NaN/Inf -> null
    }

    // string
    if let Ok(s) = obj.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }

    // bytes -> hex string
    if let Ok(b) = obj.cast::<PyBytes>() {
        let hex = crate::hex_utils::encode_hex(b.as_bytes());
        return Ok(serde_json::Value::String(hex));
    }

    // list
    if let Ok(list) = obj.cast::<PyList>() {
        return python_list_to_json(list);
    }

    // dict
    if let Ok(dict) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (key, val) in dict.iter() {
            let key_str = key.extract::<String>()?;
            let json_val = python_to_json(&val)?;
            map.insert(key_str, json_val);
        }
        return Ok(serde_json::Value::Object(map));
    }

    // Fallback: convert to string with warning
    let s = obj.str()?.to_string();
    log::warn!(
        "Converting unexpected Python type to string in JSON conversion: {s}"
    );
    Ok(serde_json::Value::String(s))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_python_none_to_json() {
        pyo3::Python::attach(|py| {
            let none = py.None();
            let json = python_to_json(&none.into_bound(py)).unwrap();
            assert!(json.is_null());
        });
    }

    #[test]
    fn test_python_bool_to_json() {
        pyo3::Python::attach(|py| {
            let true_val = true.into_pyobject(py).unwrap();
            let json = python_to_json(&true_val).unwrap();
            assert_eq!(json, serde_json::Value::Bool(true));

            let false_val = false.into_pyobject(py).unwrap();
            let json = python_to_json(&false_val).unwrap();
            assert_eq!(json, serde_json::Value::Bool(false));
        });
    }

    #[test]
    fn test_python_int_to_json() {
        pyo3::Python::attach(|py| {
            // Small positive
            let small = pyo3::types::PyInt::new(py, 42);
            let json = python_to_json(&small).unwrap();
            assert_eq!(json, serde_json::json!(42));

            // Small negative
            let neg = pyo3::types::PyInt::new(py, -100);
            let json = python_to_json(&neg).unwrap();
            assert_eq!(json, serde_json::json!(-100));

            // Large positive (doesn't fit i64)
            let large = pyo3::types::PyInt::new(py, i128::from(u64::MAX) + 1);
            let json = python_to_json(&large).unwrap();
            assert!(json.is_string());
        });
    }

    #[test]
    fn test_python_string_to_json() {
        pyo3::Python::attach(|py| {
            let s = "hello world".into_pyobject(py).unwrap();
            let json = python_to_json(&s).unwrap();
            assert_eq!(json, serde_json::Value::String("hello world".to_string()));
        });
    }

    #[test]
    fn test_python_bytes_to_json() {
        pyo3::Python::attach(|py| {
            let bytes = PyBytes::new(py, &[0x01, 0x23, 0xab, 0xcd]);
            let json = python_to_json(&bytes).unwrap();
            assert_eq!(json, serde_json::Value::String("0x0123abcd".to_string()));
        });
    }

    #[test]
    fn test_python_list_to_json() {
        pyo3::Python::attach(|py| {
            let list = PyList::new(py, [1, 2, 3]).unwrap();
            let json = python_to_json(&list).unwrap();
            assert_eq!(json, serde_json::json!([1, 2, 3]));
        });
    }

    #[test]
    fn test_python_dict_to_json() {
        pyo3::Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("key", "value").unwrap();
            let json = python_to_json(&dict).unwrap();
            assert_eq!(json, serde_json::json!({"key": "value"}));
        });
    }
}
