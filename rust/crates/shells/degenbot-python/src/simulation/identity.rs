//! The candidate-identity projection across the `PyO3` seam (ADR-098 D4).
//!
//! Rust carries one composite identity — [`degenbot_execution::CandidateId`] —
//! through every post-target record. Python receives it as ONE value, never as
//! two independent keys that could disagree: a [`PyCandidateId`] pyclass on the
//! typed records, and a nested `{"path_id", "deployment_id"}` mapping on the
//! wire dicts (the submit records and the failure/render records).

use crate::prelude::*;
use degenbot_execution::CandidateId;
use pyo3::types::PyDict;

/// The composite candidate identity, exposed as one Python value.
///
/// `path_id` is the solver's own path number (preserved verbatim);
/// `deployment_id` is the operator-assigned identity of the deployment that
/// encodes and submits this candidate.
#[pyclass(
    name = "CandidateId",
    module = "degenbot._ffi.simulation",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PyCandidateId {
    inner: CandidateId,
}

impl PyCandidateId {
    pub(crate) const fn wrap(inner: CandidateId) -> Self {
        Self { inner }
    }

    /// The Rust identity this value carries (the seam's own extraction).
    pub(crate) const fn inner(&self) -> CandidateId {
        self.inner
    }
}

#[pymethods]
impl PyCandidateId {
    /// Build a composite identity from its two halves.
    ///
    /// Python constructs one to pair with a record it composes itself (a test
    /// fixture, or a driver building a candidate from its own bookkeeping); the
    /// pipeline's own records always hand one back, never two loose keys.
    #[new]
    fn new(path_id: u64, deployment_id: u64) -> Self {
        Self {
            inner: CandidateId::new(
                path_id,
                degenbot_execution::DeploymentId::new(deployment_id),
            ),
        }
    }

    /// The solver path this candidate came from.
    #[getter]
    const fn path_id(&self) -> u64 {
        self.inner.path_id()
    }

    /// The deployment this candidate targets.
    #[getter]
    const fn deployment_id(&self) -> u64 {
        self.inner.deployment_id().value()
    }

    /// The stream form the operator-facing logs render (`path@deployment`).
    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self) -> String {
        format!(
            "CandidateId(path_id={}, deployment_id={})",
            self.inner.path_id(),
            self.inner.deployment_id()
        )
    }
}

/// Build the nested wire mapping for one candidate identity — the ONE shape the
/// dict-carrying records (submit records, failure render records) emit, so the
/// Python decoders see composite identity in exactly one place.
pub(crate) fn candidate_id_to_dict<'py>(
    py: Python<'py>,
    candidate_id: &CandidateId,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("path_id", candidate_id.path_id())?;
    dict.set_item("deployment_id", candidate_id.deployment_id().value())?;
    Ok(dict)
}
