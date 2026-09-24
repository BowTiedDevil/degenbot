//! `PyO3` bindings for the Solady `LibZip` (`FastLZ`) compress/decompress.
//!
//! Thin translator over `degenbot_core::libzip`. No business logic — extract
//! args → call core → wrap result as `HexBytes` (matching the Python oracle's
//! return type in `degenbot.utils.solady.libzip`).

use crate::prelude::*;
use pyo3::exceptions::PyTypeError;

/// Compress data using Solady's `FastLZ` algorithm.
///
/// Accepts a hex string (with optional `0x` prefix), `bytes`, `bytearray`, or
/// `HexBytes` and returns the compressed bytes as a `HexBytes`.
///
/// # Errors
///
/// Returns `PyValueError` if a hex string is malformed, or `PyTypeError` if the
/// input is neither a string nor a bytes-like object.
#[pyfunction(signature = (data))]
pub fn flz_compress<'py>(py: Python<'py>, data: &Bound<'_, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let bytes = extract_bytes_or_hex(data)?;
    // FastLZ compress is CPU-bound; release the GIL for large inputs.
    let compressed = py.detach(|| degenbot_core::libzip::compress(&bytes))?;
    cache::to_py_bytes(py, &compressed)
}

/// Decompress data using Solady's `FastLZ` algorithm.
///
/// Accepts a hex string (with optional `0x` prefix), `bytes`, `bytearray`, or
/// `HexBytes` and returns the decompressed bytes as a `HexBytes`.
///
/// # Errors
///
/// Returns `PyValueError` if a hex string is malformed or the compressed data
/// is truncated/invalid, or `PyTypeError` if the input is neither a string nor
/// a bytes-like object.
#[pyfunction(signature = (data))]
pub fn flz_decompress<'py>(
    py: Python<'py>,
    data: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let bytes = extract_bytes_or_hex(data)?;
    let decompressed = py.detach(|| degenbot_core::libzip::decompress(&bytes))?;
    cache::to_py_bytes(py, &decompressed)
}

/// Extract a byte slice from a Python `str` (hex), `bytes`/`bytearray`/`HexBytes`.
///
/// `&[u8]` extraction covers `bytes` and `HexBytes` (a `bytes` subclass) but
/// not `bytearray`, which is handled explicitly via `PyByteArray`.
fn extract_bytes_or_hex(data: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(s) = data.extract::<&str>() {
        return degenbot_core::hex_utils::decode_hex(s)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()));
    }
    if let Ok(bytes) = data.extract::<&[u8]>() {
        return Ok(bytes.to_vec());
    }
    if let Ok(ba) = data.cast::<pyo3::types::PyByteArray>() {
        return Ok(ba.to_vec());
    }
    Err(PyErr::new::<PyTypeError, _>(
        "Data must be a hex string or bytes",
    ))
}
