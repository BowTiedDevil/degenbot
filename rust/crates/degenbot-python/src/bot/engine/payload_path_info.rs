//! `PyArbEngine` — `payload_path_info` render-accessor slice (SIMPIPE2
//! T3).
//!
//! The inline-sim payload entries bypass the FFI dispatch entirely (Python
//! constructs `SubmitCandidate`s straight from the payload dicts), so the
//! `[profit]` hop-detail render needs a Python-visible `path_infos` source for
//! them. This exposes the SAME render shape `PyDispatchOutcome.path_infos`
//! emits, built from the engine's `path_info_for` projection — attribute
//! parity by construction (one serializer: `path_info_to_py_dict`).

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::simulation::outcome::path_info_to_py_dict;

use super::PyArbEngine;

#[pymethods]
impl PyArbEngine {
    /// The `[profit]`-render `path_infos` dict for ONE registered path (the
    /// same shape `DispatchOutcome.path_infos[pid]` carries) or `None` when
    /// the path is unregistered/unresolvable. SIMPIPE2 T3: payload entries
    /// skip the FFI sim, so the driver enriches merged outcomes per entry.
    ///
    /// GIL hygiene: the engine Mutex is acquired inside `with_stages` (the
    /// accessor's `py.detach`, same pattern as `inspect_path`).
    #[must_use]
    fn payload_path_info(&self, path_id: u64, py: Python<'_>) -> Option<Py<PyDict>> {
        let resolved = self
            .with_stages(py, |e| e.path_info_for(path_id))
            .and_then(std::result::Result::ok)?;
        let dict = path_info_to_py_dict(py, &resolved).ok()?;
        Some(dict.unbind())
    }
}
