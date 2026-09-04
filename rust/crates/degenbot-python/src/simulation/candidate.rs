//! `PyDispatchCandidate` — the builder the cockpit constructs from each solved
//! arb path before handing the batch to `dispatch_profitable_py` (A4).
//!
//! Mirrors [`PySubmitCandidate`]'s `#[new]` shape (a builder pyclass holding
//! the core [`DispatchCandidate`]). The candidate resolves its
//! [`composers::PathInfo`] directly from `path_id` via the
//! [`PyArbitrageEngine::path_info_for_core`] projection over the shared
//! `BotState` — no Python `PathInfo` dataclass round-trip (NXM2BF, the
//! encode-relay flatten).
//!
//! The `EncodeOptions` (`erc6909_profit` / `use_v4_batch`) come in as bool
//! kw-flags (mirrors the encode seam's signature).
//!
//! # GIL discipline
//!
//! `#[new]` runs under the GIL: it locks the engine, runs the projection
//! (a `BotState` read lock nested engine-then-core — the same order as
//! `register_path`), clones the resolved `PathInfo`, and releases. The A4
//! pyfunction then extracts the held `DispatchCandidate` (clone) into the
//! async block + releases the GIL.

use crate::bot::engine::PyArbitrageEngine;
use crate::prelude::*;
use degenbot_arbitrage::DispatchCandidate;
use degenbot_bot::solvers::arb_engine::path_info::PathInfoBuildError;
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use pyo3::exceptions::PyValueError;

/// The pre-simulation candidate builder — the engine result + the resolved
/// `PathInfo` + the encode options.
///
/// Ports the Python `EngineResult` tuple `(path_id, opt_input, profit,
/// hop_outputs, solve_block)` + the `engine_registry.paths.get(path_id)`
/// `PathInfo` resolve. Construct one per solved path, per block.
#[pyclass(
    name = "DispatchCandidate",
    skip_from_py_object,
    module = "degenbot._ffi.simulation"
)]
// `inner` is read by `dispatch_profitable_py` (A4, QQFTB4) — not yet landed;
// the field is dead until then.
pub struct PyDispatchCandidate {
    pub(crate) inner: DispatchCandidate,
}

#[pymethods]
impl PyDispatchCandidate {
    /// Build a candidate from the engine result + a registered `path_id`.
    ///
    /// The candidate resolves its `composers::PathInfo` from `path_id` via
    /// the engine's `path_info_for_core` projection over the shared
    /// `BotState` — no Python `PathInfo` dataclass is threaded (NXM2BF).
    ///
    /// Args:
    ///     `engine`: the `ArbitrageEngine` that owns `path_id` (the same
    ///         engine that produced `engine_profit`/`hop_outputs`).
    ///     `path_id`: the unique arb path identifier.
    ///     `optimal_input`: the solver's optimal swap input (u128).
    ///     `engine_profit`: the solver's expected gross profit (u128) — used
    ///         for sorting + the thin-margin filter; NOT the on-chain gross
    ///         (that's `SimResult::gross_profit`).
    ///     `hop_outputs`: the per-hop solver outputs (`list[int]`).
    ///     `consumed_inputs`: the per-hop executable inputs fed into each pool
    ///         (`list[int]`) — the solver's CL-hop clamp; the encoder feeds
    ///         these (not `hop_outputs`) as each CL pool's swap-in. May be
    ///         shorter than `hop_outputs` only in a degenerate/solver-defect
    ///         case, surfaced as `ValueError`.
    ///     `solve_block`: the block the solver produced the result on.
    ///     `state_nonces`: per-hop state nonces captured at solve time
    ///         (AV42C7 staleness gate — the dispatch seam skips candidates
    ///         whose pool state has advanced since the solve).
    ///     `erc6909_profit`: encode the V4 profit as an ERC6909 transfer
    ///         (default `False`).
    ///     `use_v4_batch`: encode V4 hops as a batched `unlock`-callback
    ///         (default `False`).
    ///
    /// # Errors
    /// `ValueError`: if `path_id` is not registered in `engine`, or a hop
    ///         has no command-stream encoder arm (Solidly / Balancer / Curve),
    ///         or `hop_outputs`/`consumed_inputs` length ≠ path hops.
    #[new]
    #[pyo3(signature = (engine, path_id, optimal_input, engine_profit, hop_outputs, consumed_inputs, solve_block, state_nonces, *, erc6909_profit=false, use_v4_batch=false))]
    #[expect(clippy::too_many_arguments, clippy::needless_pass_by_value)]
    fn new(
        py: Python<'_>,
        engine: Py<PyArbitrageEngine>,
        path_id: u64,
        optimal_input: u128,
        engine_profit: u128,
        hop_outputs: Vec<u128>,
        consumed_inputs: Vec<u128>,
        solve_block: u64,
        state_nonces: Vec<u64>,
        erc6909_profit: bool,
        use_v4_batch: bool,
    ) -> PyResult<Self> {
        let rust_path: PathInfo = engine
            .borrow(py)
            .path_info_for_core(py, path_id)
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "path_id {path_id} is not registered in this engine"
                ))
            })
            .and_then(|r| {
                r.map_err(|e: PathInfoBuildError| PyValueError::new_err(format!("{e}")))
            })?;
        // The solver returns one hop_output per hop; a mismatch is a solver
        // defect or a stale path_id. Surface as ValueError so the cockpit's
        // per-candidate try/except can skip (mirrors the pre-flatten Python
        // `len(hop_outputs) != len(path_info.hops)` guard, now Rust-side).
        if hop_outputs.len() != rust_path.hops.len() {
            return Err(PyValueError::new_err(format!(
                "hop_outputs length ({}) != path {path_id} hops ({})",
                hop_outputs.len(),
                rust_path.hops.len()
            )));
        }
        if consumed_inputs.len() != rust_path.hops.len() {
            return Err(PyValueError::new_err(format!(
                "consumed_inputs length ({}) != path {path_id} hops ({})",
                consumed_inputs.len(),
                rust_path.hops.len()
            )));
        }
        let opts = EncodeOptions {
            erc6909_profit,
            use_v4_batch,
            ..Default::default()
        };
        // Merged per-hop rows (HTPKLX 4JLQNS continuation): the Python seam
        // keeps its three-list API (adapted accessors, per the epic guardrail);
        // the core candidate stores one row per hop.
        let steps: Vec<degenbot_arbitrage::SolveStep> = hop_outputs
            .into_iter()
            .zip(consumed_inputs)
            .scan(
                state_nonces.into_iter(),
                |nonces, (output, consumed_input)| {
                    Some(degenbot_arbitrage::SolveStep {
                        output,
                        consumed_input,
                        state_nonce: nonces.next().unwrap_or(0),
                    })
                },
            )
            .collect();
        Ok(Self {
            inner: DispatchCandidate {
                path_id,
                optimal_input,
                engine_profit,
                steps: steps.into(),
                solve_block,
                path_info: rust_path,
                opts,
            },
        })
    }
}
