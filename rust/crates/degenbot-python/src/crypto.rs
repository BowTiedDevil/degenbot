//! Keccak256 + event-topic pyfunctions — the pure-crypto slice of `eth_utils`
//! owned in Rust (ergo 5JKNQH).
//!
//! `degenbot.crypto.keccak256` / `event_topic` (Python) delegate here; the
//! golden vectors in `tests/test_crypto_parity.py` pin parity with the old
//! `eth_utils` implementations byte-for-byte.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Keccak-256 (the pre-NIST Ethereum variant) of `data` as 32 raw bytes.
///
/// Args:
///     `data`: bytes to hash.
///
/// Returns:
///     The 32-byte digest as `bytes`.
#[pyfunction]
#[must_use]
pub fn keccak256(data: Vec<u8>) -> Vec<u8> {
    alloy::primitives::keccak256(&data).as_slice().to_vec()
}

/// Canonical event-signature type string of one event-input component,
/// expanding struct (tuple) params to `(...)` with recursive component
/// expansion — the same canonicalization eth-abi applied for event
/// signatures.
fn canonical_param_type(component: &Bound<'_, PyDict>) -> PyResult<String> {
    let typ: String = component
        .get_item("type")?
        .ok_or_else(|| PyValueError::new_err("event input missing 'type'"))?
        .extract()?;
    if let Some(components) = component.get_item("components")? {
        let list = components.cast::<PyList>()?;
        let inner: Vec<String> = list
            .iter()
            .map(|c| {
                let dict = c
                    .cast::<PyDict>()
                    .map_err(|_| PyValueError::new_err("event input components must be dicts"))?;
                canonical_param_type(dict)
            })
            .collect::<PyResult<_>>()?;
        // Canonical event-signature form wraps the components in a bare
        // paren-group (``(address,uint256)``), not the ``tuple(...)``
        // encoding-string form.
        return Ok(format!("({})", inner.join(",")));
    }
    Ok(typ)
}

/// Keccak256 of the canonical event signature — the standard `topic0`.
///
/// Args:
///     `event_abi`: an event ABI entry dict (`type == "event"`, `name`,
///         `inputs: [{name, type, indexed, components?}, ...]`).
///
/// Returns:
///     The 32-byte topic as `bytes`.
#[pyfunction]
pub fn event_topic(event_abi: &Bound<'_, PyDict>) -> PyResult<Vec<u8>> {
    let ty: String = event_abi
        .get_item("type")?
        .ok_or_else(|| PyValueError::new_err("event ABI entry missing 'type'"))?
        .extract()?;
    if ty != "event" {
        return Err(PyValueError::new_err("not an event ABI entry"));
    }
    let name: String = event_abi
        .get_item("name")?
        .ok_or_else(|| PyValueError::new_err("event ABI entry missing 'name'"))?
        .extract()?;
    let parts: Vec<String> = match event_abi.get_item("inputs")? {
        None => Vec::new(),
        Some(inputs) => inputs
            .cast::<PyList>()?
            .iter()
            .map(|c| {
                let dict = c
                    .cast::<PyDict>()
                    .map_err(|_| PyValueError::new_err("event inputs must be dicts"))?;
                canonical_param_type(dict)
            })
            .collect::<PyResult<_>>()?,
    };
    let signature = format!("{}({})", name, parts.join(","));
    Ok(alloy::primitives::keccak256(signature.as_bytes())
        .as_slice()
        .to_vec())
}
