//! `PyDispatchOutcome` — the public read-only view of a block's
//! `dispatch_profitable_results` outcome.
//!
//! Built by `dispatch_profitable_py` (A4) from the core [`DispatchOutcome`].
//! The `gas_profitable` getter hands back a `list[PySubmitCandidate]`
//! **directly** — that list IS the submission seam's input, so the cockpit
//! chains `dispatch_profitable_py → dispatch_and_submit_py` with no field
//! reshuffling (the ergonomic principle). `gas_unprofitable` collapses to a
//! *count* (the cockpit only logs these — they're valid sims below the net
//! threshold, not submitted, and suppression tracking happens in the core).
//!
//! Stores the core types (`Vec<SubmitCandidate>` joined from `SimResult`, +
//! `FailBuckets`) and builds Python views on getter access — mirrors how the
//! submit seam stores core types + wraps at the boundary, avoiding
//! pyclass-holding-pyclass. The `SimResult → PySubmitCandidate` join (the A4
//! pyfunction's result-wrap step) populates `gas_profitable`.
//!
//! # `PySimResult` — intentionally not a pyclass
//!
//! The plan's A2 list mentioned a `PySimResult` "internal" class. Since it
//! never crosses to Python (the join writes it straight through to
//! `PySubmitCandidate`), it is **not** a pyclass — A4 uses the core `SimResult`
//! directly. No type is exposed that the cockpit doesn't read.

use crate::executor::{path_info_to_py, HopTypes};
use crate::hex_utils::encode_hex;
use crate::prelude::*;
use crate::submission::submit::PySubmitCandidate;
use degenbot_executor::composers::PathInfo;
use degenbot_simulation::dispatch_profitable::DispatchOutcome;
use degenbot_simulation::{FailBuckets, SimFailure};
use degenbot_submission::SubmitCandidate;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;

/// The read-only outcome of a block's profitable-dispatch fan-out.
///
/// Constructed by `dispatch_profitable_py` (A4); the cockpit renders the
/// `[sim]` summary from the counters + `fail_buckets`, then hands
/// `gas_profitable` straight to `dispatch_and_submit_py`.
#[pyclass(name = "PyDispatchOutcome")]
pub struct PyDispatchOutcome {
    /// The gas-profitable candidates, joined to `SubmitCandidate` shape at
    /// result-wrap time (A4). Returned as `list[PySubmitCandidate]`.
    pub(crate) gas_profitable: Vec<SubmitCandidate>,
    pub(crate) gas_unprofitable_count: usize,
    pub(crate) exception_count: usize,
    pub(crate) fail_count: usize,
    pub(crate) candidate_count: usize,
    pub(crate) suppressed_count: usize,
    pub(crate) thin_dropped: usize,
    pub(crate) fail_buckets: FailBuckets,
    /// The per-path `SimFailure` records (one per `tally`/`record` site
    /// across the fan-out). Each carries `path_id` + the bucket label + the
    /// failing call's index in the 7-call vector (when attributable to one)
    /// + the raw revert bytes — the per-candidate detail the Python driver
    /// renders as a `[sim-fail]` line. The aggregate count lives in
    /// `fail_buckets`; this preserves per-candidate attribution.
    pub(crate) failures: Vec<SimFailure>,
    /// The input candidates' `PathInfo`, keyed by `path_id` — the join map
    /// A4 snapshots before the core consumes the batch. Populated from the
    /// INPUT batch (every candidate passed in), NOT filtered to survivors: the
    /// `[profit]` hop-detail log looks up `path_infos[cand.path_id]` per
    /// survivor, so the map must cover whatever `path_ids` the cockpit iterates
    /// (Decision 1=B, A5).
    pub(crate) path_info_by_id: HashMap<u64, PathInfo>,
}

impl PyDispatchOutcome {
    /// Build from the core `DispatchOutcome`'s joined-field tally. A4 calls
    /// this after joining each `SimResult → SubmitCandidate`.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn from_join(
        gas_profitable: Vec<SubmitCandidate>,
        path_info_by_id: HashMap<u64, PathInfo>,
        outcome: &DispatchOutcome,
    ) -> Self {
        Self {
            gas_profitable,
            path_info_by_id,
            gas_unprofitable_count: outcome.gas_unprofitable.len(),
            exception_count: outcome.exception_count,
            fail_count: outcome.fail_count,
            candidate_count: outcome.candidate_count,
            suppressed_count: outcome.suppressed_count,
            thin_dropped: outcome.thin_dropped,
            fail_buckets: outcome.fail_buckets.clone(),
            failures: outcome.failures.clone(),
        }
    }
}

#[pymethods]
impl PyDispatchOutcome {
    /// The gas-profitable candidates — `list[PySubmitCandidate]`, the direct
    /// handoff to `dispatch_and_submit_py`.
    ///
    /// Each access rebuilds the list from the held core `SubmitCandidate`s
    /// (the join source-of-truth lives in Rust; Python sees fresh wrappers).
    #[getter]
    fn gas_profitable<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for c in &self.gas_profitable {
            // Wrap the core SubmitCandidate as a PySubmitCandidate the submit
            // seam can re-extract (`dispatch_and_submit_py` does
            // `item.extract::<PyRef<PySubmitCandidate>>()`).
            let bound = Bound::new(py, PySubmitCandidate { inner: c.clone() })?;
            list.append(bound)?;
        }
        Ok(list)
    }

    #[getter]
    fn gas_unprofitable_count(&self) -> usize {
        self.gas_unprofitable_count
    }

    #[getter]
    fn exception_count(&self) -> usize {
        self.exception_count
    }

    #[getter]
    fn fail_count(&self) -> usize {
        self.fail_count
    }

    #[getter]
    fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    #[getter]
    fn suppressed_count(&self) -> usize {
        self.suppressed_count
    }

    #[getter]
    fn thin_dropped(&self) -> usize {
        self.thin_dropped
    }

    /// The revert/no-profit/overflow bucket tally — `{bucket: count}`.
    ///
    /// Drives the `[sim] … by reason: {breakdown}` summary line (rendered in
    /// Python — D4 stays-python).
    #[getter]
    fn fail_buckets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (bucket, count) in self.fail_buckets.buckets() {
            dict.set_item(bucket, *count)?;
        }
        Ok(dict)
    }

    /// The per-path simulation failure detail — `list[dict[str, Any]]`, one
    /// entry per failed candidate in fan-out order. Each dict carries:
    ///   - `path_id` (`int`),
    ///   - `bucket` (`str`) — the `classify_revert` label for reverts, else
    ///     the orchestration-only bucket (`int128-overflow`, `encode-failed`,
    ///     `rpc-failed`, `balance-decode`, `no-profit`),
    ///   - `fail_index` (`int | None`) — the index of the failing call in the
    ///     7-call vector when attributable to one (the revert branch + the
    ///     balance-decode branch); `None` for orchestration-only buckets,
    ///   - `revert_data` (`str`) — the raw revert bytes as `0x`-prefixed hex;
    ///     `"0x"` (empty) for orchestration-only buckets.
    ///
    /// Consumed by the Python companion's `[sim-fail]` renderer so the
    /// operator can identify WHICH path reverted against WHICH pools.
    /// Built at construction (the core-producing `SimFailure`s are owned,
    /// not rebuilt on access).
    #[getter]
    fn failures<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for f in &self.failures {
            let dict = PyDict::new(py);
            dict.set_item("path_id", f.path_id)?;
            dict.set_item("bucket", &f.bucket)?;
            match f.fail_index {
                Some(idx) => dict.set_item("fail_index", idx)?,
                None => dict.set_item("fail_index", py.None())?,
            }
            let hex_str = encode_hex(&f.revert_data);
            dict.set_item("revert_data", hex_str)?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// The input candidates' `PathInfo` keyed by `path_id` — `{path_id:
    /// PathInfo}`. Populated from the INPUT batch (every candidate passed in,
    /// not filtered to survivors). The `[profit]` hop-detail log looks up
    /// `path_infos[cand.path_id]` per survivor; preserved here (Decision 1=B,
    /// A5) so the cockpit doesn't thread a separate map.
    ///
    /// Lazily converts each Rust `PathInfo` to the Python `PathInfo` dataclass
    /// on access (the converter is the reverse of `extract_path_info`).
    #[getter]
    fn path_infos<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        let types = HopTypes::load(py)?;
        for (pid, path) in &self.path_info_by_id {
            dict.set_item(*pid, path_info_to_py(py, path, &types)?)?;
        }
        Ok(dict)
    }
}

// `BTreeMap` import removed: `FailBuckets` holds its bucket map
// internally and we only expose it via `buckets()` iteration.
