//! Utility functions for degenbot Rust extensions.
//!
//! Common utilities shared across modules.

use crate::fast_hexbytes::FastHexBytes;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::collections::HashSet;

/// Field names that should be converted to `FastHexBytes`.
/// These are commonly used field names for Ethereum hashes, addresses, and data.
/// Only fields that contain hex-encoded bytes should be listed here, NOT numeric fields.
fn get_hexbytes_fields() -> HashSet<&'static str> {
    [
        // Block fields
        "hash",
        "parent_hash",
        "transactions_root",
        "receipts_root",
        "state_root",
        "logs_bloom",
        "miner",
        "mix_hash",
        "nonce",
        "sha3_uncles",
        "extra_data",
        "withdrawals_root",
        // Transaction fields
        "block_hash",
        "from",
        "to",
        "input",
        "r",
        "s",
        "v",
        "blob_versioned_hashes",
        // Receipt fields
        "transaction_hash",
        "contract_address",
        // Log fields
        "address",
        "topics",
        "data",
        "block_hash",
        "transaction_hash",
    ]
    .iter()
    .copied()
    .collect()
}

/// Field names that should be converted to integers.
/// These are numeric fields that may be returned as hex strings by the JSON-RPC API.
fn get_numeric_fields() -> HashSet<&'static str> {
    [
        // Block fields
        "number",
        "timestamp",
        "gas_used",
        "gas_limit",
        "base_fee_per_gas",
        "blob_gas_used",
        "excess_blob_gas",
        "difficulty",
        "total_difficulty",
        "size",
        // Transaction fields
        "chain_id",
        "gas_price",
        "max_fee_per_gas",
        "max_priority_fee_per_gas",
        "max_fee_per_blob_gas",
        "value",
        "gas",
        // Note: nonce and transaction_index have different meanings in different contexts
        // Receipt fields
        "cumulative_gas_used",
        "effective_gas_price",
        "status",
        "transaction_type",
        // Log fields
        "block_number",
        "log_index",
        "transaction_index",
    ]
    .iter()
    .copied()
    .collect()
}

/// Check if a string value should be converted to `HexBytes`.
/// Returns true if the string starts with "0x" and contains valid hex characters.
fn is_hex_string(s: &str) -> bool {
    if s.len() < 4 {
        return false;
    }
    s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Convert a hex string to a `FastHexBytes` object.
fn hex_to_fast_hexbytes<'py>(py: Python<'py>, hex_str: &str) -> PyResult<Bound<'py, FastHexBytes>> {
    let fhb = FastHexBytes::from_hex(hex_str)?;
    fhb.into_pyobject(py)
}

/// Recursively convert hex strings to `FastHexBytes` in a Python object.
///
/// This function walks through a Python object (dict, list, or value) and converts
/// string values that look like hex strings (start with "0x") to `FastHexBytes` objects
/// if the field name is in the known `FastHexBytes` field list.
fn convert_to_hexbytes_recursive<'py>(
    py: Python<'py>,
    obj: Bound<'py, PyAny>,
    field_name: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let hexbytes_fields = get_hexbytes_fields();

    // If it's a dict, process each key-value pair
    if let Ok(dict) = obj.cast::<PyDict>() {
        let new_dict = PyDict::new(py);
        for (key, value) in dict {
            let key_str: String = key.extract()?;
            // Recursively convert the value, passing the key as context
            let converted = convert_to_hexbytes_recursive(py, value, Some(&key_str))?;
            new_dict.set_item(key_str, converted)?;
        }
        return Ok(new_dict.into_any());
    }

    // If it's a list, process each element
    if let Ok(list) = obj.cast::<PyList>() {
        let new_list = PyList::empty(py);
        for item in list.iter() {
            // For lists, check if items should be converted to FastHexBytes
            // This handles cases like "topics" which is a list of hex strings
            let should_convert = field_name.is_some_and(|name| hexbytes_fields.contains(name));

            if should_convert {
                // Try to convert string items to FastHexBytes
                if let Ok(s) = item.extract::<String>() {
                    if is_hex_string(&s) {
                        let fhb = hex_to_fast_hexbytes(py, &s)?;
                        new_list.append(fhb)?;
                        continue;
                    }
                }
            }

            // Otherwise, recursively process the item
            let converted = convert_to_hexbytes_recursive(py, item, field_name)?;
            new_list.append(converted)?;
        }
        return Ok(new_list.into_any());
    }

    // If it's a string, check if it should be converted to FastHexBytes or int
    if let Ok(s) = obj.extract::<String>() {
        let should_convert_hexbytes =
            field_name.is_some_and(|name| hexbytes_fields.contains(name) && is_hex_string(&s));

        if should_convert_hexbytes {
            return hex_to_fast_hexbytes(py, &s).map(pyo3::Bound::into_any);
        }

        // Check if this is a numeric field with a hex string value
        let should_convert_numeric =
            field_name.is_some_and(|name| get_numeric_fields().contains(name) && is_hex_string(&s));

        if should_convert_numeric {
            // Convert hex string to int
            let val = u64::from_str_radix(&s[2..], 16).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid hex number: {e}"))
            })?;
            return Ok(val.into_pyobject(py)?.into_any());
        }
    }

    // Return the object as-is
    Ok(obj)
}

/// Convert a `serde_json::Value` to a Python object with `FastHexBytes` conversion.
///
/// This function converts JSON values to their Python equivalents:
/// - `null` → `None`
/// - `bool` → `bool`
/// - `number` → `int` or `float`
/// - `string` → `str` (or `FastHexBytes` for hex strings in recognized fields)
/// - `array` → `list`
/// - `object` → `dict`
///
/// Hex strings (starting with "0x") in fields matching known Ethereum hash/address/data
/// field names are automatically converted to `FastHexBytes` objects.
///
/// # Errors
///
/// Returns `PyRuntimeError` if conversion fails for any value type.
pub fn json_to_py_with_hexbytes(
    py: Python<'_>,
    value: serde_json::Value,
) -> PyResult<Bound<'_, PyAny>> {
    // First convert JSON to basic Python object
    let py_obj = json_to_py(py, value)?;
    // Then convert hex strings to FastHexBytes
    convert_to_hexbytes_recursive(py, py_obj, None)
}

/// Convert a `serde_json::Value` to a Python object (basic conversion without `HexBytes`).
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
