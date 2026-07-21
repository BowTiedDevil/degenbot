//! `degenbot-abi` `PyO3` wrappers (`#[pyfunction]` decode/encode over the pure core).
//! Mirrors `crates/degenbot-abi/`. (ergo UG6FKN task WXHGOH.)

pub mod decoder;
pub mod encoder;

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

/// Register the ABI decode/encode functions on a real Python submodule.
///
/// Creates `degenbot._ffi.abi` (a `PyModule`, not flat root-level functions)
/// and registers the 4 fns (`decode`/`decode_single`/`encode`/`encode_single`)
/// on it. Unlike the math submodules, no de-prefixing is needed — these fns
/// were already un-prefixed; the win is the submodule + companion home
/// (importers reach them via `degenbot.abi_adapter`, not `degenbot._ffi`).
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_abi_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.abi")?;

    submod.add_function(wrap_pyfunction!(decoder::decode, &submod)?)?;
    submod.add_function(wrap_pyfunction!(decoder::decode_single, &submod)?)?;
    submod.add_function(wrap_pyfunction!(encoder::encode, &submod)?)?;
    submod.add_function(wrap_pyfunction!(encoder::encode_packed, &submod)?)?;
    submod.add_function(wrap_pyfunction!(encoder::encode_single, &submod)?)?;

    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.abi", &submod)?;

    Ok(())
}
