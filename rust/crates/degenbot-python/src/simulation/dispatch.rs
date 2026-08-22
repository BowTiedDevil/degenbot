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
//!    free — A3 `LITQFF`).
//! 3. **Join + wrap (no GIL needed).** Each surviving `SimResult` is joined
//!    to a [`SubmitCandidate`] (sim-derived fields from the result; the
//!    `executor_address` from `PySimulateContext`; the `path_pools`
//!    mutual-exclusion set derived from the originating candidate's
//!    `path_info.hops` — ports example L2476–L2478). The join is pure Rust;
//!    [`PyDispatchOutcome::from_join`] stores the core types + builds Python
//!    wrappers on getter access. No business logic in this wrapper.

use crate::prelude::*;
use crate::provider::AlloyProvider;
use crate::simulation::candidate::PyDispatchCandidate;
use crate::simulation::context::PySimulateContext;
use crate::simulation::outcome::PyDispatchOutcome;
use crate::submission::dispatcher::PyDispatcher;
use degenbot_arbitrage::BlockPriorityFees;
use degenbot_arbitrage::{dispatch_profitable_results, DispatchCandidate, DispatchOutcome};
use degenbot_arbitrage::{CapturedSwap, SimResult, SimulateContext};
use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_executor::composers::{HopInfo, PathInfo};
use degenbot_submission::{PoolKey, SubmitCandidate};
use pyo3::exceptions::PyValueError;
use pyo3::types::PyList;
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::{HashMap, HashSet};
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
#[pyo3(signature = (candidates, context, dispatcher, base_fee_next, current_block, block_timestamp, min_profit_net, min_profit_margin_bps, *, engine=None))]
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
    min_profit_net: u128,
    min_profit_margin_bps: u64,
    engine: Option<Py<crate::bot::engine::PyArbitrageEngine>>,
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
    // `.await`s (A3 `LITQFF`). The `Dispatcher` arc is NOT held during the
    // fan-out (monitor-task contention is unaffected).
    let suppression_arc = dispatcher.suppression_arc();
    // The pool-divergence arc (GMWYIU) — same standalone-arc discipline: locked
    // ONLY at the dispatch skip (step 1.5) + feedback (step 5.5) bookends in
    // the core, never across the `.await`s.
    let pool_divergence_arc = dispatcher.pool_divergence_arc();
    // The FoT registry arc (3O535Q) — same standalone-arc discipline: locked
    // ONLY at the dispatch skip (step 2.5) + feedback (step 7.5) + success
    // (step 8.5) bookends in the core, never across the `.await`s.
    let fot_registry_arc = dispatcher.fot_registry_arc();

    // ── BotState extraction (for the in-process `BlockSimHandle` path).
    // Done under the GIL: the `Py<PyArbitrageEngine>` is borrowed, the engine
    // lock is acquired (engine-then-core ordering per ADR-003), + the `core`
    // `Arc<RwLock<BotState>>` is cloned out (cheap — one Arc clone). The arc
    // threads through the async fan-out; the per-block read guard is taken in
    // the closure body (`parking_lot::RwLockReadGuard` is `Send`). When
    // `engine` is `None`, `bot_state = None` — but the legacy RPC sim path
    // retired (ADR-019 D1), so the core's `None` arm is now `unreachable!`;
    // production always supplies `engine`. Kept `Option` here transitively
    // until step 6 (HZL664) collapses the FFI seam to a required `engine`.
    //
    // `warm_cache` is the cross-block bytecode cache (`HDEG7H` Option A) —
    // cloned from the engine's `warm_code_cache_arc()` (one Arc clone, no
    // map copy). Same transitional `Option` shape as `bot_state`.
    let bot_state: Option<Arc<StateLock<degenbot_bot::bot_core::BotState>>> =
        engine.as_ref().map(|eng| eng.borrow(py).bot_state_arc());
    let warm_cache: Option<Arc<parking_lot::RwLock<degenbot_simulation::WarmCodeCacheInner>>> =
        engine
            .as_ref()
            .map(|eng| eng.borrow(py).warm_code_cache_arc());

    // ── GIL release across the per-path simulation fan-out ──
    // ergo 66H3KJ instrumentation: phase timestamps so the log shows how far
    // the dispatch future progressed if/when it deadlocks. `log::info!` here
    // goes through pyo3-log (a GIL acquire) — only at phase boundaries, so it
    // cannot itself cause the fan-out's per-candidate GIL contention; it tags
    // the start/end of the body on a tokio worker.
    let phase_candidate_count = built.len();
    tracing::info!(
        target: degenbot_bot::telemetry::DIAGNOSTIC_TARGET,
        current_block,
        phase_candidate_count,
        "[dispatch-phase] future body START (emitted synchronously — its absence \
         past this point means the GIL was already parked)"
    );
    let dispatch_body = async move {
        let phase_started = std::time::Instant::now();
        // ergo 66H3KJ phase marker: the dispatch fan-out body is about to run
        // on a tokio worker. The pyo3-log emit here is the FIRST GIL-acquire
        // the future does — if the main thread already holds the GIL
        // (build_paths sync pyo3 call / _asyncio futex park), this line will
        // NOT appear until the GIL frees; its absence in the log vs the
        // `[dispatch-phase] future body START` line above pinpoints the block.
        tracing::info!(
            target: degenbot_bot::telemetry::DIAGNOSTIC_TARGET,
            current_block,
            phase_candidate_count,
            "[dispatch-phase] fan-out ENTER"
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
            min_profit_net,
            min_profit_margin_bps,
            &pool_divergence_arc,
            &fot_registry_arc,
            bot_state,
            warm_cache,
        );
        tracing::info!(
            target: degenbot_bot::telemetry::DIAGNOSTIC_TARGET,
            current_block,
            elapsed_ms = %phase_started.elapsed().as_millis(),
            survivors = outcome.gas_profitable.len(),
            "[dispatch-phase] fan-out EXIT"
        );

        // ── Join survivors → SubmitCandidates (pure Rust — no GIL needed) ──
        // The cockpit chains dispatch_profitable_py → dispatch_and_submit_py
        // straight through PyDispatchOutcome.gas_profitable, so the join
        // produces exactly the field set dispatch_and_submit consumes.
        //
        // Ergo 63I7WJ: collect each survivor's `captured_swaps` in parallel —
        // the success-path surface the step-5 classifier re-points at (the
        // revert path already surfaces them via `failures()` on each
        // `SimFailure`). `SimResult.captured_swaps` is the swap-event capture
        // drained from the inspector after `execute()`.
        let joined: Vec<SubmitCandidate> = outcome
            .gas_profitable
            .iter()
            .map(|r| join_sim_result(r, &path_info_by_id, executor_address))
            .collect();
        let success_captured_swaps: Vec<(u64, Vec<CapturedSwap>)> = outcome
            .gas_profitable
            .iter()
            .map(|r| (r.path_id, r.captured_swaps.clone()))
            .collect();

        // ergo 66H3KJ phase marker: the future body has produced its
        // outcome and is about to return. pyo3-async-runtimes then schedules
        // `spawn_blocking(|| Python::attach(set_result))` to hand the result to
        // the awaiting asyncio future — that `Python::attach` is the SECOND
        // GIL acquire on this dispatch path, and the one the parked main
        // thread starves. If this line appears but the next block never
        // advances, the result-setter is the blocked step.
        tracing::info!(
            target: degenbot_bot::telemetry::DIAGNOSTIC_TARGET,
            current_block,
            "[dispatch-phase] future body END — handing to set_result via Python::attach"
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
    let dispatch_span = tracing::info_span!(
        "degenbot.simulate.dispatch",
        current_block,
        phase_candidate_count
    );
    future_into_py(py, dispatch_body.instrument(dispatch_span))
}

/// Join a [`SimResult`] + its originating path hops → a [`SubmitCandidate`].
///
/// The [`SimResult`] carries the sim-derived fields (gross/net profit, gas
/// used UN-inflated, priority fee, base fee, execute calldata, access list);
/// the originating candidate's `path_info.hops` supply the mutual-exclusion
/// `path_pools` set. The `executor_address` comes from `PySimulateContext`
/// (the session-static executor contract — identical for every candidate in a
/// fan-out).
///
/// `gas_used` is the simulate's raw `gasUsed` (UN-inflated); the 1.5×
/// `inflated_gas()` safety margin is computed downstream at submit time
/// (assigned to `tx_params["gas"]`), not stored on the candidate — matches
/// [`PySubmitCandidate`]'s `gas_used` contract.
///
/// If the originating candidate can't be recovered (a `path_id` that wasn't in
/// the input batch — should never happen — the core only re-emits `path_ids` it
/// was handed), the `path_pools` set is empty (a degenerate but safe
/// fall-through: isolation falls back to no mutual-exclusion for that one
/// path; under normal operation the lookup always hits).
fn join_sim_result(
    r: &SimResult,
    path_info_by_id: &HashMap<u64, PathInfo>,
    executor_address: alloy::primitives::Address,
) -> SubmitCandidate {
    let path_pools = path_info_by_id
        .get(&r.path_id)
        .map(|p| derive_path_pools(&p.hops))
        .unwrap_or_default();
    SubmitCandidate {
        path_id: r.path_id,
        gross_profit: r.gross_profit,
        net_profit: r.net_profit,
        gas_used: r.gas_used, // UN-inflated (1.5× applied at submit time).
        priority_fee: r.priority_fee,
        base_fee_next: r.base_fee_next,
        execute_calldata: r.execute_calldata.clone(),
        executor_address,
        access_list: r.access_list.clone(),
        path_pools,
    }
}

/// Derive the mutual-exclusion pool-key set from a path's hops.
///
/// Ports `examples/eth_backrun_v2_v3_v4_rust.py` L2476–L2478:
/// `{h.pool_id_hex if isinstance(h, V4HopInfo) else h.pool_address for h in
/// path_info.hops}`. V4 → the `pool_id_hex` string (the 0x-prefixed salted
/// id); V2/V3 → the checksummed `pool_address` Display form (alloy
/// `Address::Display` produces EIP-55, matching the Python cockpit's
/// `h.pool_address`). The set is the dispatcher's mutual-exclusion key —
/// `is_path_blocked` / `reserve_pools` test set-membership + equality.
fn derive_path_pools(hops: &[HopInfo]) -> HashSet<PoolKey> {
    hops.iter()
        .map(|h| match h {
            HopInfo::V4(v4) => PoolKey::new(v4.pool_id_hex.clone()),
            HopInfo::V2(v2) => PoolKey::new(format!("{}", v2.pool_address)),
            HopInfo::V3(v3) => PoolKey::new(format!("{}", v3.pool_address)),
        })
        .collect()
}
