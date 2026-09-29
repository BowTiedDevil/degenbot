//! Batched engine-result → `DispatchCandidate` assembly.
//!
//! The settlement runner shaped each raw solver result row (the named
//! `RawEngineResult` record) into a `DispatchCandidate` one Python object at
//! a time. This seam performs the whole batch in one call: payload-served
//! path ids and empty-hop rows are skipped, the survivors resolve their
//! `composers::PathInfo` through the same `PyArbEngine::path_info_for_core`
//! projection the single-candidate builder uses, and the returned
//! [`PyCandidateAssembly`] carries the ready candidates plus the empty-hop
//! path ids the driver logs.
//!
//! Since the core batch executor landed, the per-row policy is crate-owned:
//! the skip predicates live in the crate's `classify_raw_row`, and the shape
//! guard and candidate construction in its `build_raw_candidate` — the ONE
//! home the core executor's stage 1 and this seam both drive. This pyfunction
//! is the thin ADR-013 delegate: boundary extraction, crate call, pyclass
//! wrap. The resolve is the one engine-bound step, kept here with its own
//! error fidelity (`PathInfoBuildError` messages survive). A resolve miss
//! re-raises the legacy `ValueError`: decision (a) makes the raw-row miss a
//! typed `SkipResolveMiss`, and the pre-cut-over Python driver surfaces it
//! as an error until the cut-over task consumes the typed records.
//!
//! The mapping stays in the simulation binding domain rather than inside the
//! pyo3-free core: the rows are solver/simulation results + an engine
//! `BotState` projection, and the resolve reaches the engine wrapper.

use crate::bot::engine::PyArbEngine;
use crate::prelude::*;
use crate::simulation::candidate::PyDispatchCandidate;
use degenbot_arbitrage::DispatchCandidate;
use degenbot_batch_executor::assembly::{build_raw_candidate, classify_raw_row, RawRowClass};
use degenbot_batch_executor::record::AssemblyVerdict;
use degenbot_batch_executor::row::RawResult;
use degenbot_bot::arb_engine::path_info::PathInfoBuildError;
use degenbot_executor::composers::EncodeOptions;
use pyo3::exceptions::PyValueError;
use pyo3::types::PyList;
use std::collections::HashSet;

/// One raw engine-result row — the named record the runner constructs at the
/// batch-stream conversion point (`_consume._engine_result`) and this seam
/// extracts by field name (`FromPyObject` getattr extraction over the frozen
/// dataclass); no positional 7-tuple crosses the boundary.
#[derive(FromPyObject, Clone)]
pub struct RawEngineResult {
    pub(crate) path_id: u64,
    pub(crate) optimal_input: u128,
    pub(crate) engine_profit: u128,
    pub(crate) hop_outputs: Vec<u128>,
    pub(crate) consumed_inputs: Vec<u128>,
    pub(crate) solve_block: u64,
    pub(crate) state_nonces: Vec<u64>,
}

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
/// The skip predicates and the shape guard + candidate construction are
/// crate-owned (see the module docs); this delegate resolves each surviving
/// row's path through the engine and wraps the result.
///
/// # Errors
/// `ValueError`: a `path_id` is not registered in `engine`, a resolved path
/// failed its `PathInfoBuildError` projection, or a row's
/// `hop_outputs`/`consumed_inputs`/`state_nonces` length does not match the
/// path's hop count (the crate's loud-abort shape guard).
#[pyfunction]
#[pyo3(signature = (engine, results, *, erc6909_profit=false, use_v4_batch=false, skip_path_ids=None))]
#[expect(clippy::needless_pass_by_value)] // the engine handle is borrowed for the batch
pub fn assemble_dispatch_candidates_py(
    py: Python<'_>,
    engine: Py<PyArbEngine>,
    results: Vec<RawEngineResult>,
    erc6909_profit: bool,
    use_v4_batch: bool,
    skip_path_ids: Option<Vec<u64>>,
) -> PyResult<PyCandidateAssembly> {
    let skip: HashSet<u64> = skip_path_ids.unwrap_or_default().into_iter().collect();
    let opts = EncodeOptions {
        erc6909_profit,
        use_v4_batch,
        ..Default::default()
    };
    let mut candidates = Vec::with_capacity(results.len());
    let mut empty_hop_path_ids = Vec::new();

    for row in results {
        let raw = RawResult {
            path_id: row.path_id,
            optimal_input: row.optimal_input,
            profit: row.engine_profit,
            hop_outputs: row.hop_outputs,
            consumed_inputs: row.consumed_inputs,
            solve_block: row.solve_block,
            state_nonces: row.state_nonces,
        };
        match classify_raw_row(&raw, &skip) {
            RawRowClass::Skip(AssemblyVerdict::SkipEmptyHops) => {
                // An empty hop list is not an encodable path — the sim seam
                // never sees it. Reported so the driver's `[sim-none]` log
                // keeps its home.
                empty_hop_path_ids.push(raw.path_id);
            }
            RawRowClass::Skip(AssemblyVerdict::SkipPayloadServed) => {
                // Already simulated inline by the engine; its submit record
                // comes from the payload arm, not the FFI sim batch.
            }
            RawRowClass::Skip(other) => {
                unreachable!("no other pre-resolve skip exists, got {other:?}")
            }
            RawRowClass::Build => {
                // The engine-bound resolve (its build error keeps its own
                // message — fidelity the crate's `Option` resolver trait
                // cannot carry).
                let path_info = engine
                    .borrow(py)
                    .path_info_for_core(py, raw.path_id)
                    .ok_or_else(|| {
                        PyValueError::new_err(format!(
                            "path_id {} is not registered in this engine",
                            raw.path_id
                        ))
                    })
                    .and_then(|r| {
                        r.map_err(|e: PathInfoBuildError| PyValueError::new_err(format!("{e}")))
                    })?;
                // The crate's loud-abort shape guard + candidate construction.
                let candidate = build_raw_candidate(&raw, &path_info, opts)
                    .map_err(|e| PyValueError::new_err(e.detail))?;
                candidates.push(candidate);
            }
        }
    }

    Ok(PyCandidateAssembly {
        candidates,
        empty_hop_path_ids,
    })
}
