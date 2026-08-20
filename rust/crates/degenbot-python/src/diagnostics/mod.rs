//! Diagnostics instrumentation: GIL-probe + main-loop stuck-watchdog
//! (ergo 66H3KJ). See [`gil_probe`] for the deadlock measurement rationale.

pub mod gil_probe;
pub mod thread_registry;

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Register the diagnostics pyfunctions on the module.
///
/// # Errors
/// Returns `PyErr` if a function fails to register.
pub fn add_diagnostics_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    gil_probe::add_diagnostics_module(m)
}
