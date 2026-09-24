//! Batched engine-result → `DispatchCandidate` assembly.
//!
//! The settlement runner shaped each raw solver result row
//! `(path_id, optimal_input, engine_profit, hop_outputs, consumed_inputs,
//! solve_block, state_nonces)` into a `DispatchCandidate` one Python object at
//! a time. This seam performs the whole batch in one call: payload-served
//! path ids and empty-hop rows are skipped, the survivors resolve their
//! `composers::PathInfo` through the same `PyArbEngine::path_info_for_core`
//! projection the single-candidate builder uses, and the returned
//! [`PyCandidateAssembly`] carries the ready candidates plus the empty-hop
//! path ids the driver logs.
//!
//! The mapping lives here, in the simulation binding domain, rather than in
//! `degenbot-submission`: the rows are solver/simulation results + an engine
//! `BotState` projection, and the output is the pre-sim candidate the sim
//! fan-out consumes — not the submission crate's post-sim `SubmitCandidate`.

use crate::bot::engine::PyArbEngine;
use crate::prelude::*;
use crate::simulation::candidate::{build_dispatch_candidate, PyDispatchCandidate};
use degenbot_arbitrage::DispatchCandidate;
use pyo3::types::PyList;
use std::collections::HashSet;

/// One raw engine-result row — the tuple shape the solver result batch stream
/// delivers.
type RawResultRow = (u64, u128, u128, Vec<u128>, Vec<u128>, u64, Vec<u64>);

/// The batched assembly result: ready candidates + the skipped empty-hop path
/// ids (the display-only `[sim-none]` log's input).
#[pyclass(name = "CandidateAssembly", module = "degenbot._ffi.simulation")]
pub struct PyCandidateAssembly {
    /// The assembled candidates, in input-row order.
    pub(crate) candidates: Vec<DispatchCandidate>,
    /// Path ids skipped because their row carried no hop outputs.
    pub(crate) empty_hop_path_ids: Vec<u64>,
}

#[pymethods]
impl PyCandidateAssembly {
    /// The assembled candidates as `list[DispatchCandidate]` (the sim seam's
    /// input shape), in input-row order.
    #[getter]
    fn candidates<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for c in &self.candidates {
            list.append(Bound::new(py, PyDispatchCandidate { inner: c.clone() })?)?;
        }
        Ok(list)
    }

    /// Path ids skipped for empty hop outputs — the driver logs each.
    #[getter]
    fn empty_hop_path_ids(&self) -> Vec<u64> {
        self.empty_hop_path_ids.clone()
    }
}

/// Assemble a batch of raw engine-result rows into `DispatchCandidate`s.
///
/// Rows with no hop outputs are skipped and reported through
/// [`PyCandidateAssembly::empty_hop_path_ids`]; rows whose `path_id` is in
/// `skip_path_ids` (already simulated inline by the engine) are dropped
/// silently. This is the batched replacement for the runner's per-row Python
/// construction, and the single home for the field mapping.
///
/// # Errors
/// `ValueError`: a `path_id` is not registered in `engine`, or a row's
/// `hop_outputs`/`consumed_inputs` length does not match the path's hop count.
#[pyfunction]
#[pyo3(signature = (engine, results, *, erc6909_profit=false, use_v4_batch=false, skip_path_ids=None))]
#[expect(clippy::needless_pass_by_value)] // the engine handle is borrowed for the batch
pub fn assemble_dispatch_candidates_py(
    py: Python<'_>,
    engine: Py<PyArbEngine>,
    results: Vec<RawResultRow>,
    erc6909_profit: bool,
    use_v4_batch: bool,
    skip_path_ids: Option<Vec<u64>>,
) -> PyResult<PyCandidateAssembly> {
    let skip: HashSet<u64> = skip_path_ids.unwrap_or_default().into_iter().collect();
    let mut candidates = Vec::with_capacity(results.len());
    let mut empty_hop_path_ids = Vec::new();

    for (
        path_id,
        optimal_input,
        engine_profit,
        hop_outputs,
        consumed_inputs,
        solve_block,
        state_nonces,
    ) in results
    {
        // An empty hop list is not an encodable path — the sim seam never
        // sees it. Reported so the driver's `[sim-none]` log keeps its home.
        if hop_outputs.is_empty() {
            empty_hop_path_ids.push(path_id);
            continue;
        }
        // Already simulated inline by the engine; its submit record comes
        // from the payload arm, not the FFI sim batch.
        if skip.contains(&path_id) {
            continue;
        }
        candidates.push(build_dispatch_candidate(
            py,
            &engine,
            path_id,
            optimal_input,
            engine_profit,
            hop_outputs,
            consumed_inputs,
            solve_block,
            state_nonces,
            erc6909_profit,
            use_v4_batch,
        )?);
    }

    Ok(PyCandidateAssembly {
        candidates,
        empty_hop_path_ids,
    })
}
