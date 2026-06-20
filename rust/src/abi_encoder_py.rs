//! `PyO3` wrappers for ABI encoding.
//!
//! Thin binding layer over [`degenbot_abi::abi_encoder`] (the pure-Rust core).
//! The core `encode_rust` / `encode_single_rust` / `encode_for_types` functions
//! live in the `degenbot-abi` workspace member and have no `PyO3` dependency;
//! this module adds the `#[pyfunction]` entry points registered in
//! [`crate::lib`]'s `#[pymodule]`. Python-value extraction delegates to
//! [`crate::alloy_py::abi_value_from_python`].

use degenbot_abi::abi_encoder::{encode_rust, encode_single_rust};
use degenbot_abi::abi_types::AbiValue;
use pyo3::{exceptions::PyValueError, prelude::*, types::PyBytes, types::PyList};

use crate::alloy_py;

// =============================================================================
// PyO3-exposed functions (thin wrappers)
// =============================================================================

/// Encode a single ABI value.
///
/// # Arguments
///
/// * `abi_type` - ABI type string (e.g., "uint256", "address", "bytes")
/// * `value` - The value to encode
///
/// # Returns
///
/// The ABI-encoded bytes.
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
pub fn encode_single<'py>(
    py: Python<'py>,
    abi_type: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    // SAFETY: GIL is held — `abi_value_from_python` extracts values from
    // Python objects that require the GIL. The subsequent `py.detach()` releases
    // the GIL for the pure-Rust encode computation.
    let abi_value = alloy_py::abi_value_from_python(py, value)?;
    let abi_type_owned = abi_type.to_string();
    let encoded = py
        .detach(|| encode_single_rust(&abi_type_owned, &abi_value))
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    Ok(PyBytes::new(py, &encoded))
}

/// Encode multiple ABI values.
///
/// # Arguments
///
/// * `types` - List of ABI type strings
/// * `values` - List of Python values to encode
///
/// # Returns
///
/// The ABI-encoded bytes.
#[allow(clippy::missing_errors_doc)]
#[pyfunction]
#[pyo3(signature = (types, values))]
pub fn encode<'py>(
    py: Python<'py>,
    types: &Bound<'_, PyList>,
    values: &Bound<'_, PyList>,
) -> PyResult<Bound<'py, PyBytes>> {
    if types.len() != values.len() {
        return Err(PyValueError::new_err(format!(
            "Type count {} does not match value count {}",
            types.len(),
            values.len()
        )));
    }

    // SAFETY: GIL is held — `abi_value_from_python` extracts values from
    // Python objects that require the GIL. The subsequent `py.detach()` releases
    // the GIL for the pure-Rust encode computation.
    let abi_values: Result<Vec<AbiValue>, _> = values
        .iter()
        .map(|v| alloy_py::abi_value_from_python(py, &v))
        .collect();

    let type_strings: Vec<String> = types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;

    let type_refs: Vec<&str> = type_strings.iter().map(String::as_str).collect();

    let abi_values = abi_values?;
    let encoded = py
        .detach(|| encode_rust(&type_refs, &abi_values))
        .map_err(|e| PyValueError::new_err(format!("{e}")))?;
    Ok(PyBytes::new(py, &encoded))
}

// Note: the pure-Rust roundtrip proptests (uint256/address/bool/bytes32/int256
// roundtrips, `test_encode_*`, `test_roundtrip`) live in `degenbot-abi`'s
// `abi_encoder` module — no Python needed.
