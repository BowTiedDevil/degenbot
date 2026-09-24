//! Solady `LibZip` (`FastLZ`) `PyO3` wrappers over the pure `degenbot_core::libzip`
//! core. Mirrors the core surface; no per-domain feature gate (the libzip code
//! lives in `degenbot-core`, which is always a dependency).

use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::wrap_pyfunction;

pub mod libzip;

/// Register the Solady `LibZip` functions on a real Python submodule.
///
/// Creates `degenbot._ffi.solady` (a `PyModule`, not flat root-level
/// functions) and registers `flz_compress` / `flz_decompress` on it.
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function`/`sys.modules` call fails.
pub fn add_solady_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.solady")?;
    submod.add_function(wrap_pyfunction!(libzip::flz_compress, &submod)?)?;
    submod.add_function(wrap_pyfunction!(libzip::flz_decompress, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.solady", &submod)?;
    Ok(())
}
