//! `dispatch_profitable_py` — the `PyO3` seam over the core
//! [`dispatch_profitable_results`] fan-out (the D-row capstone leaf).
//!
//! Ports the Python `dispatch_profitable_results` orchestrator
//! (`examples/eth_backrun_v2_v3_v4_rust.py`, the L2450–L2535 fan-out +
//! categorization) — NOT as a re-implementation but as the thin driver shell
//! over the already-ported Rust core. The cockpit swaps its `await
//! dispatch_profitable_results(...)` Python call for `await
//! dispatch_profitable_py(...)`; the [sim] summary rendering (the
//! `format_failure_breakdown` log) stays Python (D4 `stays-python`).
//!
//! # GIL discipline (ADR-005 §3 C)
//!
//! Mirrors [`dispatch_and_submit_py`] exactly (arg-extract → `py.detach` →
//! core call → wrap):
//!
//! 1. **GIL-held arg extraction.** Walk the `list[PyDispatchCandidate]`,
//!    cloning each held core [`DispatchCandidate`] into the input batch +
//!    snapshotting `path_id → PathInfo` into a join map (the post-fan-out
//!    `SimResult → PySubmitCandidate` join needs the hops; `SimResult` carries
//!    only `hop_count`). Clone the `Arc<AlloyProvider>` + the addresses +
//!    the runtime bytecode + the warmup off `PySimulateContext`. Resolve
//!    `Option<BlockPriorityFees>` from the dispatcher's priority-fee ring
//!    (the latest recorded block's p10/p50 — ports the Python
//!    `dispatcher.block_priority_fees[max(...)]` + `.get(10)/.get(50)`
//!    lookup the cockpit did inline pre-A4). Take the suppression arc.
//! 2. **GIL release across the per-path simulation fan-out.** `future_into_py`
//!    runs the future on the tokio runtime the Python event loop drives; the
//!    GIL is NOT held while the per-tx `eth_simulateV1` /
//!    `eth_createAccessList` RPCs block on the network. The `SimulateContext`
//!    borrows the moved `Arc<AlloyProvider>` (block-local: the `'a` borrow is
//!    alive for the `dispatch_profitable_results(...).await` only). The
//!    suppression arc is locked ONLY at the dispatch bookends
//!    (pre-filter + outcome record) inside the core — NEVER held across the
//!    `buffer_unordered` `.await`s — and the `Dispatcher` arc is not locked
//!    at all during the fan-out (the monitor tasks that contend for it stay
//!    free.
//! 3. **Join + wrap (no GIL needed).** Each surviving `SimResult` is joined
//!    to a [`SubmitCandidate`] (sim-derived fields from the result; the
//!    `executor_address` from `PySimulateContext`; the `path_pools`
//!    mutual-exclusion set derived from the originating candidate's
//!    `path_info.hops` — ports example L2476–L2478). The join is pure Rust;
//!    [`PyDispatchOutcome::from_join`] stores the core types + builds Python
//!    wrappers on getter access. No business logic in this wrapper.

use crate::ambient_runtime::future_into_py;
use crate::prelude::*;
use crate::provider::AlloyProvider;
use crate::simulation::candidate::PyDispatchCandidate;
use crate::simulation::context::PySimulateContext;
use crate::simulation::outcome::{path_info_to_py_dict, PyDispatchOutcome};
use crate::submission::dispatcher::PyDispatcher;
use degenbot_arbitrage::BlockPriorityFees;
use degenbot_arbitrage::{dispatch_profitable_results, DispatchCandidate, DispatchOutcome};
use degenbot_arbitrage::{CapturedSwap, SimulateContext};
use degenbot_batch_executor::assembly::{join_sim_result, merge_payload_results, PayloadArm};
use degenbot_batch_executor::row::{PayloadFailure, PayloadRow};
use degenbot_batch_executor::PathResolver;
use degenbot_core::op_info;
use degenbot_executor::composers::PathInfo;
use degenbot_submission::SubmitCandidate;
use degenbot_substrate::state_lock::StateLock;
use pyo3::exceptions::PyValueError;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Instrument as _;

/// The signature re-exported so the `#[pyo3(signature)]` reference stays in
/// sync with the exposed type (mirrors the convention in `submit.rs`).
///
/// # Errors
///
/// `ValueError`: if `candidates` holds a non-`PyDispatchCandidate` element
/// (the GIL-held arg extraction rejects it before the async block is created),
/// None — the core `dispatch_profitable_results` is infallible (every
/// per-path failure is tallied into `outcome.fail_buckets`, not propagated).
///
/// # Panics
///
/// Panics if the dispatcher or suppression mutex is poisoned (a peer task
/// panicked while holding it). Cannot happen under normal operation; a poison
/// indicates a bug in a sibling task (the dispatcher/suppression mutexes are
/// only ever locked for short synchronous spans).
#[pyfunction]
#[pyo3(signature = (candidates, context, dispatcher, base_fee_next, current_block, block_timestamp, min_profit_margin_bps, *, engine=None))]
#[expect(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]
pub fn dispatch_profitable_py<'py>(
    py: Python<'py>,
    candidates: &Bound<'_, PyList>,
    context: &PySimulateContext,
    dispatcher: &PyDispatcher,
    base_fee_next: u128,
    current_block: u64,
    block_timestamp: u64,
    min_profit_margin_bps: u64,
    engine: Option<Py<crate::bot::engine::PyArbEngine>>,
) -> PyResult<Bound<'py, PyAny>> {
    // ── GIL-held arg extraction ──
    // Walk the candidate list: clone each held DispatchCandidate into the
    // input batch (the core consumes the Vec by value) AND snapshot
    // path_id → PathInfo into a join map (the post-fan-out SimResult →
    // SubmitCandidate join needs the hops; the SimResult carries only
    // hop_count).
    let mut built: Vec<DispatchCandidate> = Vec::with_capacity(candidates.len());
    let mut path_info_by_id: HashMap<u64, PathInfo> = HashMap::with_capacity(candidates.len());
    for item in candidates.iter() {
        let c = item
            .extract::<PyRef<'_, PyDispatchCandidate>>()
            .map_err(|_| {
                PyValueError::new_err("candidates must be a list of PyDispatchCandidate instances")
            })?;
        path_info_by_id.insert(c.inner.path_id, c.inner.path_info.clone());
        built.push(c.inner.clone());
    }

    // SimulateContext borrows the provider (cloned arc — the 'a borrow is
    // block-local to the future).
    //
    // Incident 2026-08-20: the core fan-out holds a BotState READ guard
    // across these provider fetches, and GIL-held FFI writers park behind
    // that reader (holding the GIL while parked). The default 30s x N-attempt
    // retry budget under the guard stalled the GIL for minutes; the sim path
    // uses the fail-fast `sim_bounded` budget instead (slow cold miss =>
    // `rpc-failed` tally, no multi-second guard hold).
    let provider: Arc<AlloyProvider> = Arc::new(context.provider.sim_bounded());
    let executor_owner = context.executor_owner;
    let executor_address = context.executor_address;
    let weth_address = context.weth_address;
    let pool_manager_address = context.pool_manager_address;
    let multicall3_address = context.multicall3_address;
    let inject_code = context.inject_code;
    let injected_address = context.injected_address;
    let runtime_bytecode = context.runtime_bytecode.clone();
    let warmup = context.warmup;

    // Resolve the per-block priority-fee percentiles from the dispatcher's
    // priority-fee ring (the latest recorded block's p10/p50). Ports the
    // Python `dispatcher.block_priority_fees[max(...)]` + `.get(10)/.get(50)`
    // + the `if block_priority_fees:` falsy → None branch the cockpit did
    // inline before the A4 port. `None` when nothing recorded yet (matches
    // the Python empty-dict behavior — no panic).
    let dispatcher_arc = dispatcher.inner_arc();
    let block_priority_fees: Option<BlockPriorityFees> = {
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let guard = dispatcher_arc.lock().expect("dispatcher mutex poisoned");
        guard
            .block_priority_fees()
            .last_key_value()
            .map(|(&block, fees)| BlockPriorityFees {
                block,
                p10: alloy::primitives::U256::from(*fees.get(&10).unwrap_or(&0)),
                p50: alloy::primitives::U256::from(*fees.get(&50).unwrap_or(&0)),
            })
    };

    // The suppression arc — locked ONLY at the dispatch bookends inside the
    // core (pre-filter + outcome record); NEVER held across the fan-out
    // `.await`s). The `Dispatcher` arc is NOT held during the
    // fan-out (monitor-task contention is unaffected).
    let suppression_arc = dispatcher.suppression_arc();
    // The pool-divergence arc — same standalone-arc discipline: locked
    // ONLY at the dispatch skip (step 1.5) + feedback (step 5.5) bookends in
    // the core, never across the `.await`s.
    let pool_divergence_arc = dispatcher.pool_divergence_arc();
    // The FoT registry arc — same standalone-arc discipline: locked
    // ONLY at the dispatch skip (step 2.5) + feedback (step 7.5) + success
    // (step 8.5) bookends in the core, never across the `.await`s.
    let fot_registry_arc = dispatcher.fot_registry_arc();

    // ── BotState extraction (for the in-process `BlockSimHandle` path).
    // Done under the GIL: the `Py<PyArbEngine>` is borrowed, the engine
    // lock is acquired (engine-then-core ordering per ADR-003), + the `core`
    // `Arc<RwLock<BotState>>` is cloned out (cheap — one Arc clone). The arc
    // threads through the async fan-out; the per-block read guard is taken in
    // the closure body (`parking_lot::RwLockReadGuard` is `Send`). When
    // `engine` is `None`, `bot_state = None` — but the legacy RPC sim path
    // retired (ADR-019 D1), so the core's `None` arm is now `unreachable!`;
    // production always supplies `engine`. Kept `Option` here transitively
    // until step 6 collapses the FFI seam to a required `engine`.
    //
    // `warm_cache` is the cross-block bytecode cache (Option A) —
    // cloned from the engine's `warm_code_cache_arc()` (one Arc clone, no
    // map copy). Same transitional `Option` shape as `bot_state`.
    let bot_state: Option<Arc<StateLock<degenbot_substrate::BotState>>> =
        engine.as_ref().map(|eng| eng.borrow(py).bot_state_arc());
    let warm_cache: Option<Arc<parking_lot::RwLock<degenbot_simulation::WarmCodeCacheInner>>> =
        engine
            .as_ref()
            .map(|eng| eng.borrow(py).warm_code_cache_arc());

    // ── GIL release across the per-path simulation fan-out ──
    // Deadlock diagnostics: phase timestamps so the log shows how far
    // the dispatch future progressed if/when it deadlocks. `log::info!` here
    // goes through pyo3-log (a GIL acquire) — only at phase boundaries, so it
    // cannot itself cause the fan-out's per-candidate GIL contention; it tags
    // the start/end of the body on a tokio worker.
    let phase_candidate_count = built.len();
    op_info!(
        domain = sim,
        current_block,
        phase_candidate_count,
        "future body START (emitted synchronously — its absence \
         past this point means the GIL was already parked)"
    );
    let dispatch_body = async move {
        let phase_started = std::time::Instant::now();
        // Phase marker: the dispatch fan-out body is about to run
        // on a tokio worker. The pyo3-log emit here is the FIRST GIL-acquire
        // the future does — if the main thread already holds the GIL
        // (build_paths sync pyo3 call / _asyncio futex park), this line will
        // NOT appear until the GIL frees; its absence in the log vs the
        // `[dispatch-phase] future body START` line above pinpoints the block.
        op_info!(
            domain = sim,
            current_block,
            phase_candidate_count,
            "fan-out ENTER"
        );
        let ctx = SimulateContext {
            provider: &provider,
            executor_owner,
            executor_address,
            weth_address,
            pool_manager_address,
            multicall3_address,
            inject_code,
            injected_address,
            runtime_bytecode,
            warmup,
            base_fee_next,
            current_block,
            block_timestamp,
            block_priority_fees,
        };
        let outcome: DispatchOutcome = dispatch_profitable_results(
            built,
            &ctx,
            &suppression_arc,
            current_block,
            min_profit_margin_bps,
            &pool_divergence_arc,
            &fot_registry_arc,
            bot_state,
            warm_cache,
        );
        op_info!(domain = sim, current_block,
            elapsed_ms = %phase_started.elapsed().as_millis(),
            survivors = outcome.gas_profitable.len(),
            "fan-out EXIT"
        );

        // ── Join survivors → SubmitCandidates (pure Rust — no GIL needed) ──
        // The cockpit chains dispatch_profitable_py → dispatch_and_submit_py
        // straight through PyDispatchOutcome.gas_profitable, so the join
        // produces exactly the field set dispatch_and_submit consumes.
        //
        // Collect each survivor's `captured_swaps` in parallel —
        // the success-path surface the step-5 classifier re-points at (the
        // revert path already surfaces them via `failures()` on each
        // `SimFailure`). `SimResult.captured_swaps` is the swap-event capture
        // drained from the inspector after `execute()`.
        let joined: Vec<SubmitCandidate> = outcome
            .gas_profitable
            .iter()
            .map(|r| join_sim_result(r, path_info_by_id.get(&r.path_id), executor_address))
            .collect();
        let success_captured_swaps: Vec<(u64, Vec<CapturedSwap>)> = outcome
            .gas_profitable
            .iter()
            .map(|r| (r.path_id, r.captured_swaps.clone()))
            .collect();

        // Phase marker: the future body has produced its
        // outcome and is about to return. pyo3-async-runtimes then schedules
        // `spawn_blocking(|| Python::attach(set_result))` to hand the result to
        // the awaiting asyncio future — that `Python::attach` is the SECOND
        // GIL acquire on this dispatch path, and the one the parked main
        // thread starves. If this line appears but the next block never
        // advances, the result-setter is the blocked step.
        op_info!(
            domain = sim,
            current_block,
            "future body END — handing to set_result via Python::attach"
        );
        Ok(PyDispatchOutcome::from_join(
            joined,
            path_info_by_id,
            &outcome,
            success_captured_swaps,
        ))
    };
    // Telemetry (2026-08-22 audit): ONE Jaeger span per simulate fan-out.
    // `.instrument` (not a held `enter()` guard) is mandatory — this future
    // hops tokio workers, and a thread-local guard would strand the span
    // context on the wrong thread. The GIL-probe phase markers ride it as
    // events on the degenbot::diag target (capped off the console sinks).
    // f701ccd3 bridge: the span is built by `degenbot-bot::telemetry`, which
    // re-attaches the published block's span context as a remote parent so
    // the Rust→Python handoff stays trace-continuous.
    let dispatch_span =
        degenbot_bot::telemetry::simulate_dispatch_span(current_block, phase_candidate_count);
    future_into_py(py, dispatch_body.instrument(dispatch_span))
}

// ─────────────────────────────────────────────────────────────────────────
// The inline-sim payload seam — one sim seam for both entry arms
// ─────────────────────────────────────────────────────────────────────────
//
// The engine's inline-sim payloads (SIMPIPE2 T3) used to be re-categorized
// in Python (_dispatch.py::_merge_payload_outcome): a per-hop JSON set
// comprehension re-derived the mutual-exclusion pool keys
// (derive_path_pools' mirror) and the payload net profit was re-compared
// against the MIN_PROFIT_NET threshold value. Both facts are Rust-owned —
// this seam reuses the FFI batch's own join (join_sim_result →
// derive_path_pools) + the core's is_gas_profitable predicate so the policy
// is evaluated by one rule, here, for both arms. Python renders the returned
// rows/verdicts only (the _dispatch.py docstring's contract).

/// A resolved-map bridge over the crate's `PathResolver`: the GIL-held
/// resolve phase (below) feeds it, and the crate merge consumes it — plain
/// map data, so it is `Send + Sync` where a GIL token cannot be.
struct ResolvedPaths<'a> {
    map: &'a HashMap<u64, PathInfo>,
}

impl PathResolver for ResolvedPaths<'_> {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.map.get(&path_id).cloned()
    }
}

/// The payload arm of the sim seam: derive the dispatch-policy
/// facts for the engine's inline-sim payload records Rust-side, through the
/// SAME row builder the FFI batch join uses.
///
/// Each payload dict is the `result_channel` serializer's primitive field-set
/// (`path_id` / `gross_profit` / `net_profit` / `gas_used` / `priority_fee` /
/// `base_fee_next` / `execute_calldata` / `access_list` / `failure`). Per entry:
///
/// 1. The registered path's typed hops are resolved via the SAME
///    engine projection the FFI batch candidates use
///    (`PyArbEngine::path_info_for_core`) and routed through
///    `join_sim_result` → `derive_path_pools` — the mutual-exclusion set is
///    byte-identical to the FFI batch row for the same path id, by
///    construction (V4 → `pool_id_hex`; V2/V3 → EIP-55 Display).
/// 2. Categorization applies the core's `is_gas_profitable` predicate ONCE
///    (the same `MIN_PROFIT_NET` floor the FFI batch fan-out applies: a net
///    at or above the floor submits; below counts gas-unprofitable). A
///    failure payload becomes a sim-fail row.
///    Python receives the verdict as a `kind` string only — it never reads
///    the threshold value.
/// 3. The boundary extracts the payload's primitive fields into the crate
///    `PayloadRow`, and the crate-owned join reassembles the core row, so the
///    submit row shape is the
///    exact `PyDispatchOutcome.gas_profitable` handoff.
///
/// Args:
///     payloads: list of payload dicts (one per inline-sim entry).
///     engine: the `PyArbEngine` (the typed-hop resolver — the same
///         engine the payload-producing result batch came from).
///     `executor_address`: the session executor contract (the join stamps it
///         identically on every row, like `dispatch_profitable_py` does).
///
/// Returns:
///     `PayloadOutcome` — the merged record set Python renders (submit rows,
///     unprofitable tally, sim-fail rows, `path_infos`, captured swaps are
///     empty on the payload arm: the engine's inspector capture rides the
///     render dict, not a `CapturedSwap` vector).
///
/// # Errors
///
/// `ValueError`: a payload dict is missing a required field, the calldata is
/// not bytes, or a `path_id` is not registered in the engine (the pool-key
/// derivation needs the typed hops).
#[pyfunction]
#[pyo3(signature = (payloads, engine, executor_address))]
pub fn merge_payload_results_py(
    py: Python<'_>,
    payloads: &Bound<'_, PyList>,
    engine: &crate::bot::engine::PyArbEngine,
    executor_address: &str,
) -> PyResult<PyPayloadOutcome> {
    let executor = crate::address_utils::parse_address(executor_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid executor address: {e}")))?;

    // Boundary extraction only: each payload dict → the crate's `PayloadRow`.
    // The join + categorization policy is crate-owned
    // (`degenbot_batch_executor::assembly::merge_payload_results`) — the ONE
    // home the core batch executor's stage 1 and this seam both drive.
    let rows: Vec<PayloadRow> = payloads
        .iter()
        .map(|item| {
            let entry = item
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("payloads must be a list of payload dicts"))?;
            payload_row_from_dict(entry)
        })
        .collect::<PyResult<Vec<_>>>()?;

    // Resolve phase (GIL-held): one registered PathInfo per DISTINCT payload
    // path_id through the engine projection. A resolve miss here is decision
    // (a)'s loud-abort arm — a payload row is engine-born (the engine
    // simulated the path inline), so a miss evidences batch/registry
    // divergence and aborts — surfacing as the legacy `ValueError` (the
    // pre-cut-over driver's contract), with the crate merge's message.
    let mut resolved: HashMap<u64, PathInfo> = HashMap::with_capacity(rows.len());
    for row in &rows {
        if resolved.contains_key(&row.path_id) {
            continue;
        }
        let info = engine
            .path_info_for_core(py, row.path_id)
            .and_then(std::result::Result::ok)
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    row.path_id
                ))
            })?;
        resolved.insert(row.path_id, info);
    }

    // The join + categorization run crate-side over the resolved map.
    let merged = merge_payload_results(&rows, &ResolvedPaths { map: &resolved }, executor)
        .map_err(|e| PyValueError::new_err(e.detail))?;

    // The [profit]-render path_info dicts — the ONE serializer shape
    // (`path_info_to_py_dict`) both outcomes' path_infos emit, in
    // first-appearance order over DISTINCT path ids.
    let path_info_dicts: Vec<(u64, Py<PyDict>)> = merged
        .path_infos
        .iter()
        .map(|(path_id, info)| Ok((*path_id, path_info_to_py_dict(py, info)?.unbind())))
        .collect::<PyResult<Vec<_>>>()?;
    // Python switches its render branch on `kind` and reads nothing else —
    // the threshold comparison ran crate-side.
    let verdicts = merged
        .verdicts
        .iter()
        .map(|(path_id, arm)| {
            let kind = match arm {
                PayloadArm::Submit => "submit",
                PayloadArm::Unprofitable => "unprofitable",
            };
            PyPayloadVerdict::wrap(py, *path_id, kind)
        })
        .collect();

    Ok(PyPayloadOutcome {
        candidates: merged.submits,
        unprofitable_count: merged.unprofitable_count,
        failures: merged.failures,
        path_info_dicts,
        verdicts,
    })
}

/// Extract a required u64 field off one payload dict (seam helper).
fn required_u64(entry: &Bound<'_, PyDict>, key: &str) -> PyResult<u64> {
    entry
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("payload dict missing '{key}'")))?
        .extract()
        .map_err(|_| PyValueError::new_err(format!("payload field '{key}' must be an int")))
}

/// Extract one payload dict → the crate's `PayloadRow` — the boundary
/// translation (field names + Python types) only. The `access_list` rows the
/// serializer emitted (the EIP-2930 JSON shape) are parsed back into the
/// alloy `AccessList` so the crate join stamps the row exactly like the FFI
/// batch survivor.
pub(crate) fn payload_row_from_dict(entry: &Bound<'_, PyDict>) -> PyResult<PayloadRow> {
    let path_id = required_u64(entry, "path_id")?;
    let gross_obj = entry
        .get_item("gross_profit")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'gross_profit'"))?;
    let gross_profit: alloy::primitives::U256 =
        crate::conversion::alloy::extract_python_u256(&gross_obj)?;
    let net_obj = entry
        .get_item("net_profit")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'net_profit'"))?;
    let net_profit: alloy::primitives::U256 =
        crate::conversion::alloy::extract_python_u256(&net_obj)?;
    let calldata = entry
        .get_item("execute_calldata")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'execute_calldata'"))?;
    let calldata_bytes: &[u8] = calldata
        .cast::<PyBytes>()
        .map_err(|_| PyValueError::new_err("payload field 'execute_calldata' must be bytes"))?
        .as_bytes();

    let access_list = match entry.get_item("access_list")? {
        Some(v) if !v.is_none() => Some(parse_payload_access_list(&v)?),
        _ => None,
    };

    let failure = match entry.get_item("failure")? {
        Some(fobj) if !fobj.is_none() => {
            let fdict = fobj
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("payload field 'failure' must be a dict"))?;
            Some(payload_failure_from_dict(fdict)?)
        }
        _ => None,
    };

    Ok(PayloadRow {
        path_id,
        gross_profit,
        net_profit,
        gas_used: required_u64(entry, "gas_used")?,
        priority_fee: required_u128(entry, "priority_fee")?,
        base_fee_next: required_u128(entry, "base_fee_next")?,
        execute_calldata: alloy::primitives::Bytes::copy_from_slice(calldata_bytes),
        access_list,
        failure,
    })
}

/// Parse the payload's access-list rows (the EIP-2930 JSON shape
/// `result_channel` emits: [{`address`, `storageKeys`: ["0x…32-byte hex", …]}, …])
/// into the alloy `AccessList` (the same decode the submission seam's
/// `parse_access_list` applies to `PySubmitCandidate`'s optional rows).
fn parse_payload_access_list(v: &Bound<'_, PyAny>) -> PyResult<alloy::rpc::types::AccessList> {
    let entries = v
        .try_iter()?
        .map(|row_any| {
            let row_ob = row_any?;
            let row = row_ob
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("access-list row must be a dict"))?;
            let addr_str: String = row
                .get_item("address")?
                .ok_or_else(|| PyValueError::new_err("access-list row missing 'address'"))?
                .extract()?;
            let address: alloy::primitives::Address =
                degenbot_core::address_utils::parse_address(&addr_str)
                    .map_err(|e| PyValueError::new_err(format!("bad access-list address: {e}")))?;
            let mut storage_keys: Vec<alloy::primitives::FixedBytes<32>> = Vec::new();
            if let Some(keys) = row.get_item("storageKeys")? {
                if !keys.is_none() {
                    for key in keys.try_iter()? {
                        let key = key.map_err(|_| {
                            PyValueError::new_err("storageKeys must be a list of hex strings")
                        })?;
                        let hex: String = key.extract().map_err(|_| {
                            PyValueError::new_err("storage key must be a hex string")
                        })?;
                        let bytes = alloy::primitives::hex::decode(&hex)
                            .map_err(|e| PyValueError::new_err(format!("bad storage key: {e}")))?;
                        let fb = alloy::primitives::FixedBytes::<32>::try_from(bytes.as_slice())
                            .map_err(|_| PyValueError::new_err("storage key must be 32 bytes"))?;
                        storage_keys.push(fb);
                    }
                }
            }
            Ok::<_, PyErr>(alloy::rpc::types::AccessListItem {
                address,
                storage_keys,
            })
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(alloy::rpc::types::AccessList(entries))
}

/// Extract a required u128 field off one payload dict (seam helper).
fn required_u128(entry: &Bound<'_, PyDict>, key: &str) -> PyResult<u128> {
    entry
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("payload dict missing '{key}'")))?
        .extract()
        .map_err(|_| PyValueError::new_err(format!("payload field '{key}' must be an int")))
}

/// Build the crate's payload-failure record from the payload's failure
/// sub-dict — the FFI failures row keys (`bucket` / `fail_index` /
/// `revert_data`; the path id rides on the row). The revert bytes stay
/// `Bytes` here; the Python-facing hex string renders at the getter.
fn payload_failure_from_dict(failure: &Bound<'_, PyDict>) -> PyResult<PayloadFailure> {
    let bucket = match failure.get_item("bucket")? {
        Some(v) if !v.is_none() => v.extract::<String>()?,
        _ => "inline-fail".to_string(),
    };
    let fail_index = match failure.get_item("fail_index")? {
        Some(v) if !v.is_none() => Some(v.extract::<usize>().map_err(|_| {
            PyValueError::new_err("payload field 'fail_index' must be an int or null")
        })?),
        _ => None,
    };
    let revert_data = match failure.get_item("revert_data")? {
        Some(v) if !v.is_none() => {
            if let Ok(s) = v.extract::<String>() {
                let bytes =
                    alloy::primitives::hex::decode(s.trim_start_matches("0x")).map_err(|e| {
                        PyValueError::new_err(format!(
                            "payload field 'revert_data' is not hex: {e}"
                        ))
                    })?;
                alloy::primitives::Bytes::from(bytes)
            } else {
                let bytes: &[u8] = v.extract().map_err(|_| {
                    PyValueError::new_err("payload field 'revert_data' must be hex str or bytes")
                })?;
                alloy::primitives::Bytes::copy_from_slice(bytes)
            }
        }
        _ => alloy::primitives::Bytes::new(),
    };
    Ok(PayloadFailure {
        bucket,
        fail_index,
        revert_data,
    })
}

/// The per-entry categorization verdict — Python switches its render
/// branch on kind and reads nothing else. (submit / unprofitable; failure
/// entries surface through the failures rows, not a verdict.)
impl PyPayloadVerdict {
    fn wrap(py: Python<'_>, path_id: u64, kind: &'static str) -> Py<PyPayloadVerdict> {
        #[expect(clippy::unwrap_used)] // PyResult from Bound::new for a plain
        // pyclass cannot fail (no __new__ override, no GC-tracked fields).
        Py::new(py, PyPayloadVerdict { path_id, kind }).unwrap()
    }
}
#[pyclass(name = "PayloadVerdict", module = "degenbot._ffi.simulation")]
pub struct PyPayloadVerdict {
    /// The payload's path id.
    pub(crate) path_id: u64,
    /// "submit" | "unprofitable".
    pub(crate) kind: &'static str,
}

#[pymethods]
impl PyPayloadVerdict {
    /// The path id the verdict belongs to.
    #[getter]
    fn path_id(&self) -> u64 {
        self.path_id
    }

    /// "submit" | "unprofitable" — the arm tag (the threshold comparison ran
    /// Rust-side; Python renders the branch, it does not re-apply the rule).
    #[getter]
    fn kind(&self) -> &'static str {
        self.kind
    }
}

/// The merged inline-sim payload record set — the payload arm of
/// `PyDispatchOutcome`, built by `merge_payload_results_py`. Rust owns every
/// policy fact; Python renders + stitches these getters into the merged
/// outcome view.
#[pyclass(name = "PayloadOutcome", module = "degenbot._ffi.simulation")]
pub struct PyPayloadOutcome {
    /// The joined submit rows (the exact `PyDispatchOutcome.gas_profitable`
    /// element type — `dispatch_and_submit` re-extracts them unchanged).
    pub(crate) candidates: Vec<SubmitCandidate>,
    /// Valid sims below the threshold (Rust-categorized).
    pub(crate) unprofitable_count: usize,
    /// The [sim-fail] rows (FFI failures-row shape).
    pub(crate) failures: Vec<(u64, PayloadFailure)>,
    /// The [profit]-render `path_info` dicts (`path_info_to_py_dict` shape).
    pub(crate) path_info_dicts: Vec<(u64, Py<PyDict>)>,
    /// The per-entry arm verdicts (`path_id` + `kind`).
    pub(crate) verdicts: Vec<Py<PyPayloadVerdict>>,
}

#[pymethods]
impl PyPayloadOutcome {
    /// The submit rows — `list[SubmitCandidate]`, the `gas_profitable`
    /// handoff shape (each element re-extracts as a `PySubmitCandidate`).
    #[getter]
    fn candidates<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for c in &self.candidates {
            let bound = Bound::new(
                py,
                crate::submission::submit::PySubmitCandidate { inner: c.clone() },
            )?;
            list.append(bound)?;
        }
        Ok(list)
    }

    /// Valid sims whose net profit fell below the threshold — the merged
    /// outcome's `gas_unprofitable_count` slice.
    #[getter]
    fn unprofitable_count(&self) -> usize {
        self.unprofitable_count
    }

    /// The [sim-fail] rows — list[dict] with the FFI failures row keys
    /// (`path_id` / `bucket` / `fail_index` / `revert_data`).
    #[getter]
    fn failures<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (path_id, failure) in &self.failures {
            let dict = PyDict::new(py);
            dict.set_item("path_id", path_id)?;
            dict.set_item("bucket", &failure.bucket)?;
            match failure.fail_index {
                Some(idx) => dict.set_item("fail_index", idx)?,
                None => dict.set_item("fail_index", py.None())?,
            }
            // The revert bytes render as plain hex — the inline-sim payload
            // arm's historical shape (empty bytes → the empty string).
            dict.set_item(
                "revert_data",
                alloy::primitives::hex::encode(&failure.revert_data),
            )?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// `{path_id: path_info dict}` — the [profit]-render source (the SAME
    /// one-serializer shape `PyDispatchOutcome.path_infos` emits).
    #[getter]
    fn path_infos<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (pid, info) in &self.path_info_dicts {
            dict.set_item(pid, info.bind(py))?;
        }
        Ok(dict)
    }

    /// The per-entry verdicts — `list[PayloadVerdict]` (`path_id` + `kind`).
    #[getter]
    fn verdicts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for v in &self.verdicts {
            list.append(v.bind(py))?;
        }
        Ok(list)
    }
}
