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

use crate::hex_utils::encode_hex;
use crate::prelude::*;
use crate::submission::submit::PySubmitCandidate;
use alloy::primitives::U256;
use degenbot_arbitrage::DispatchOutcome;
use degenbot_arbitrage::{CapturedSwap, FailBuckets, SimFailure};
use degenbot_executor::composers::{HopInfo, PathInfo};
use degenbot_submission::SubmitCandidate;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;

/// The read-only outcome of a block's profitable-dispatch fan-out.
///
/// Constructed by `dispatch_profitable_py` (A4); the cockpit renders the
/// `[sim]` summary from the counters + `fail_buckets`, then hands
/// `gas_profitable` straight to `dispatch_and_submit_py`.
#[pyclass(name = "DispatchOutcome", module = "degenbot._ffi.simulation")]
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
    /// The per-call pool-divergence skip count (ergo `GMWYIU`) — candidates
    /// dropped pre-sim because they routed through a pool flagged `SolverCalc`
    /// within the decay window. Mirrors `thin_dropped` / `suppressed_count`;
    /// the lifetime tally lives on `PyDispatcher.total_divergent_dropped`.
    pub(crate) divergent_dropped: usize,
    /// The per-call `FoT` skip count (ergo `3O535Q`) — candidates dropped
    /// pre-sim because a hop's input token was FoT-confirmed. Mirrors
    /// `divergent_dropped`; the lifetime tally lives on
    /// `PyDispatcher.total_fot_dropped`.
    pub(crate) fot_dropped: usize,
    pub(crate) fail_buckets: FailBuckets,
    /// The per-path `SimFailure` records (one per `tally`/`record` site
    /// across the fan-out). Each carries `path_id` + the bucket label + the
    /// failing call's index in the 7-call vector (when attributable to one)
    /// + the raw revert bytes — the per-candidate detail the Python driver
    /// renders as a `[sim-fail]` line. The aggregate count lives in
    /// `fail_buckets`; this preserves per-candidate attribution.
    pub(crate) failures: Vec<SimFailure>,
    /// The SUCCESS-path captured swaps, one entry per profitable survivor,
    /// keyed by `path_id` (ergo epic 63I7WJ). The revert-path swaps already
    /// ride on each `SimFailure` (surfaced via `failures()`); this is the
    /// matching success-path surface so the step-5 classifier re-point can
    /// consume the decoded swap amounts instead of the `diagnostic.rs`
    /// onchain recompute (`decode_swap_log(event).amount == solver.hop_outputs[i]`
    /// — the getAmountOut recompute is redundant once these cross the FFI).
    pub(crate) success_captured_swaps: Vec<(u64, Vec<CapturedSwap>)>,
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
    pub(crate) fn from_join(
        gas_profitable: Vec<SubmitCandidate>,
        path_info_by_id: HashMap<u64, PathInfo>,
        outcome: &DispatchOutcome,
        success_captured_swaps: Vec<(u64, Vec<CapturedSwap>)>,
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
            divergent_dropped: outcome.divergent_dropped,
            fot_dropped: outcome.fot_dropped,
            fail_buckets: outcome.fail_buckets.clone(),
            failures: outcome.failures.clone(),
            success_captured_swaps,
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

    /// The per-call pool-divergence skip count (ergo `GMWYIU`) — candidates
    /// dropped pre-sim because they routed through a pool flagged `SolverCalc`
    /// within the decay window. Mirrors `thin_dropped` / `suppressed_count`.
    #[getter]
    fn divergent_dropped(&self) -> usize {
        self.divergent_dropped
    }

    /// The per-call candidate count dropped because a hop's input token was
    /// FoT-confirmed (ergo `3O535Q`).
    #[getter]
    fn fot_dropped(&self) -> usize {
        self.fot_dropped
    }

    /// The SUCCESS-path captured swaps — `list[dict]`, one entry per
    /// profitable survivor, each dict carrying `path_id` (`int`) +
    /// `captured_swaps` (`list[dict]` of per-swap `family`/`emitter`/
    /// `amount0`/`amount1`/`sqrt_price_x96`/`liquidity`/`tick`).
    ///
    /// The matching success-path surface to each `SimFailure.captured_swaps`
    /// the revert path surfaces via `failures()`. Each entry's swap list is
    /// the V2/V3/V4 `Swap` events the in-process EVM emitted during the
    /// survivor's `execute()` — the ground-truth hop output (no
    /// `getAmountOut` recompute, no Multicall3 reserves re-fetch needed). The
    /// step-5 classifier (`logs/permutation_analyzer.py` +
    /// `format_sim_diag_line`) re-points at these to retire
    /// `diagnostic.rs::recompute_v2/v3_amount_out`. Empty for survivors that
    /// swapped zero pools (shouldn't happen — `encode_cmd_stream` only builds
    /// swap commands for paths with hops).
    #[getter]
    fn profitable_captured_swaps<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (path_id, swaps) in &self.success_captured_swaps {
            let dict = PyDict::new(py);
            dict.set_item("path_id", *path_id)?;
            let swaps_list = PyList::empty(py);
            for s in swaps {
                swaps_list.append(captured_swap_to_dict(py, s)?)?;
            }
            dict.set_item("captured_swaps", swaps_list)?;
            list.append(dict)?;
        }
        Ok(list)
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
    ///   - `reverting_frame` (`dict | None`) — the inspector-captured deep
    ///     attribution for `execute()` reverts: `depth` (`int`), `target`
    ///     (`str` address), `selector` (`str` 4-byte hex), `revert_data`
    ///     (`str` hex), `label` (`str`). `None` for orchestration-only buckets
    ///     + the balance-decode branch (ergo epic 63I7WJ task 3AJ4I4).
    ///   - `captured_swaps` (`list[dict]`) — the V2/V3/V4 swap events captured
    ///     before the revert (per-swap `family`/`emitter`/`amount0`/`amount1`/"+ "
    ///     `sqrt_price_x96`/`liquidity`/`tick`). Empty for orchestration-only
    ///     buckets + the balance-decode branch (ergo epic 63I7WJ task SUD5UT).
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
            match &f.reverting_frame {
                Some(rf) => {
                    let rdict = PyDict::new(py);
                    rdict.set_item("depth", rf.depth)?;
                    rdict.set_item("target", format!("{:#x}", rf.target))?;
                    rdict.set_item(
                        "selector",
                        format!("0x{}", alloy::primitives::hex::encode(rf.selector)),
                    )?;
                    rdict.set_item("revert_data", encode_hex(&rf.revert_data))?;
                    rdict.set_item("label", &rf.label)?;
                    rdict.set_item("outcome_kind", rf.outcome_kind)?;
                    rdict.set_item("gas_used", rf.gas_used)?;
                    dict.set_item("reverting_frame", rdict)?;
                }
                None => dict.set_item("reverting_frame", py.None())?,
            }
            // The captured swaps (before the revert) — per-swap dicts.
            let swaps_list = PyList::empty(py);
            for s in &f.captured_swaps {
                swaps_list.append(captured_swap_to_dict(py, s)?)?;
            }
            dict.set_item("captured_swaps", swaps_list)?;
            dict.set_item("log_full_count", f.log_full_count)?;
            // Swaps emitted inside REVERTED frames (frame-misclassification diag).
            let rsw = PyList::empty(py);
            for s in &f.reverted_swaps {
                rsw.append(captured_swap_to_dict(py, s)?)?;
            }
            dict.set_item("reverted_swaps", rsw)?;
            // Compact per-frame EVM call-trace summary (no-profit diagnostic).
            let ct_list = PyList::empty(py);
            for s in &f.call_trace {
                ct_list.append(s)?;
            }
            dict.set_item("call_trace", ct_list)?;
            dict.set_item("weth_before", f.weth_before)?;
            dict.set_item("weth_after", f.weth_after)?;
            dict.set_item("eth_before", f.eth_before)?;
            dict.set_item("eth_after", f.eth_after)?;
            dict.set_item("erc6909_before", f.erc6909_before)?;
            dict.set_item("erc6909_after", f.erc6909_after)?;
            // The solver's expected amounts — the [sim-diag] classifier's
            // EXPECTED half (the ACTUAL half is `captured_swaps`). The gap
            // between `hop_outputs[i]` and the i-th captured swap's amount
            // is the new SolverCalc basis (replaces the deleted recompute).
            // `optimal_input` is the solver's expected input (context for the
            // expected-vs-actual render). Ergo epic 63I7WJ task AM5AJW.
            let optimal_input = alloy_py::u256_to_py(py, &U256::from(f.optimal_input))?;
            dict.set_item("optimal_input", optimal_input)?;
            let hop_outputs_list = PyList::empty(py);
            for ho in &f.hop_outputs {
                let v = alloy_py::u256_to_py(py, &U256::from(*ho))?;
                hop_outputs_list.append(v)?;
            }
            dict.set_item("hop_outputs", hop_outputs_list)?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// The input candidates' `PathInfo` keyed by `path_id` — `{path_id:
    /// dict}`. Populated from the INPUT batch (every candidate passed in,
    /// not filtered to survivors). The `[profit]` hop-detail log looks up
    /// `path_infos[cand.path_id]` per survivor; preserved here (Decision 1=B,
    /// A5) so the cockpit doesn't thread a separate map.
    ///
    /// Each value is a plain `dict` (WEFVGE — the Python `hop_info`
    /// dataclass render type retired):
    ///   - `path_type` (`str`) — the combined pool-type label,
    ///   - `hops` (`list[dict]`) — one dict per hop, carrying a `family`
    ///     discriminator (`"V2"`/`"V3"`/`"V4"`) + that variant's fields.
    ///
    /// Built directly from the Rust `HopInfo`s (no Python dataclass
    /// reconstruction — `path_info_to_py`/`hop_to_py` retired).
    #[getter]
    fn path_infos<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (pid, path) in &self.path_info_by_id {
            dict.set_item(*pid, path_info_to_py_dict(py, path)?)?;
        }
        Ok(dict)
    }
}

/// Build a plain-Python-dict view of a `PathInfo` (the render shape, WEFVGE).
///
/// `path_type` is the combined pool-type label (`"V2-V3"`, `"V4-V2"`, …);
/// `hops` is a list of per-hop dicts built by [`hop_to_py_dict`]. No Python
/// dataclass is reconstructed — the cockpit reads the dict fields directly.
fn path_info_to_py_dict<'py>(py: Python<'py>, path: &PathInfo) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    let mut type_names: Vec<&'static str> = Vec::with_capacity(path.hops.len());
    let hops_list = PyList::empty(py);
    for hop in &path.hops {
        let (family, hop_dict) = hop_to_py_dict(py, hop)?;
        type_names.push(family);
        hops_list.append(hop_dict)?;
    }
    dict.set_item("path_type", type_names.join("-"))?;
    dict.set_item("hops", hops_list)?;
    Ok(dict)
}

/// Build a plain-Python-dict view of a `HopInfo` (the per-hop render shape).
///
/// Returns the `family` discriminator alongside the dict so the caller
/// (`path_info_to_py_dict`) can assemble the `path_type` label without reading
/// the dict back. The dict carries that variant's display fields; addresses
/// are EIP-55 checksummed strings (alloy `Address::Display`) — matches the
/// pre-flatten `hop_to_py` output. Kept in `simulation/outcome.rs` (the render
/// home); the executor seam's `hop_to_py`/`path_info_to_py` dataclass
/// constructors are retired.
fn hop_to_py_dict<'py>(
    py: Python<'py>,
    hop: &HopInfo,
) -> PyResult<(&'static str, Bound<'py, PyDict>)> {
    let dict = PyDict::new(py);
    let family = match hop {
        HopInfo::V2(v2) => {
            dict.set_item("family", "V2")?;
            dict.set_item("pool_address", format!("{}", v2.pool_address))?;
            dict.set_item("token0_address", format!("{}", v2.token0_address))?;
            dict.set_item("token1_address", format!("{}", v2.token1_address))?;
            dict.set_item("fee", v2.fee)?;
            dict.set_item("zfo", v2.zfo)?;
            "V2"
        }
        HopInfo::V3(v3) => {
            dict.set_item("family", "V3")?;
            dict.set_item("pool_address", format!("{}", v3.pool_address))?;
            dict.set_item("token0_address", format!("{}", v3.token0_address))?;
            dict.set_item("token1_address", format!("{}", v3.token1_address))?;
            dict.set_item("fee", v3.fee)?;
            dict.set_item("zfo", v3.zfo)?;
            "V3"
        }
        HopInfo::V4(v4) => {
            dict.set_item("family", "V4")?;
            dict.set_item(
                "pool_manager_address",
                format!("{}", v4.pool_manager_address),
            )?;
            dict.set_item("pool_id_hex", v4.pool_id_hex.clone())?;
            dict.set_item("currency0_address", format!("{}", v4.currency0_address))?;
            dict.set_item("currency1_address", format!("{}", v4.currency1_address))?;
            dict.set_item("fee", v4.fee)?;
            dict.set_item("tick_spacing", v4.tick_spacing)?;
            dict.set_item("hook_address", format!("{}", v4.hook_address))?;
            dict.set_item("zfo", v4.zfo)?;
            "V4"
        }
    };
    Ok((family, dict))
}

// `BTreeMap` import removed: `FailBuckets` holds its bucket map
// internally and we only expose it via `buckets()` iteration.

/// Build the per-swap dict for a captured swap (family/emitter/amount0/amount1/
/// `sqrt_price_x96/liquidity/tick`) — the shared shape the revert-path
/// `failures()` + the success-path `profitable_captured_swaps()` getters both
/// emit, so the Python consumer sees one captured-swap dict shape regardless of
/// whether the swap came from a reverted or a profitable run (ergo epic 63I7WJ).
pub(crate) fn captured_swap_to_dict<'py>(
    py: Python<'py>,
    s: &CapturedSwap,
) -> PyResult<Bound<'py, PyDict>> {
    let sdict = PyDict::new(py);
    sdict.set_item("family", format!("{:?}", s.family).to_lowercase())?;
    sdict.set_item("emitter", format!("{:#x}", s.emitter))?;
    let amount0 = alloy_py::i256_to_py(py, &s.amount0)?;
    sdict.set_item("amount0", amount0)?;
    let amount1 = alloy_py::i256_to_py(py, &s.amount1)?;
    sdict.set_item("amount1", amount1)?;
    let sqrt_price = alloy_py::u256_to_py(py, &s.sqrt_price_x96)?;
    sdict.set_item("sqrt_price_x96", sqrt_price)?;
    let liquidity = alloy_py::u256_to_py(py, &s.liquidity)?;
    sdict.set_item("liquidity", liquidity)?;
    sdict.set_item("tick", s.tick)?;
    Ok(sdict)
}
