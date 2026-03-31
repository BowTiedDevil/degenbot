//! Utility functions for degenbot Rust extensions.
//!
//! Common utilities shared across modules.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Convert a `serde_json::Value` to a Python object.
///
/// This function converts JSON values to their Python equivalents:
/// - `null` → `None`
/// - `bool` → `bool`
/// - `number` → `int` or `float`
/// - `string` → `str`
/// - `array` → `list`
/// - `object` → `dict`
///
/// # Errors
///
/// Returns `PyRuntimeError` if conversion fails for any value type.
pub fn json_to_py(py: Python<'_>, value: serde_json::Value) -> PyResult<Bound<'_, PyAny>> {
    match value {
        serde_json::Value::Null => Ok(py.None().into_bound(py)),
        serde_json::Value::Bool(b) => {
            let py_bool = b
                .into_pyobject(py)
                .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("Failed to convert bool"))?;
            Ok(Bound::clone(&py_bool).into_any())
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)
                    .map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("Failed to convert i64")
                    })?
                    .into_any())
            } else if let Some(u) = n.as_u64() {
                Ok(u.into_pyobject(py)
                    .map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("Failed to convert u64")
                    })?
                    .into_any())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)
                    .map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("Failed to convert f64")
                    })?
                    .into_any())
            } else {
                Ok(py.None().into_bound(py))
            }
        }
        serde_json::Value::String(s) => Ok(s
            .into_pyobject(py)
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("Failed to convert string"))?
            .into_any()),
        serde_json::Value::Array(arr) => {
            let py_list = PyList::empty(py);
            for item in arr {
                py_list.append(json_to_py(py, item)?)?;
            }
            Ok(py_list.into_any())
        }
        serde_json::Value::Object(map) => {
            let py_dict = PyDict::new(py);
            for (key, value) in map {
                py_dict.set_item(key, json_to_py(py, value)?)?;
            }
            Ok(py_dict.into_any())
        }
    }
}
