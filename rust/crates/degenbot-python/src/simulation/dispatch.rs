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
use degenbot_executor::composers::{HopInfo, PathInfo};
use degenbot_simulation::dispatch_profitable::{
    dispatch_profitable_results, DispatchCandidate, DispatchError, DispatchOutcome,
};
use degenbot_simulation::simulate_one::{SimResult, SimulateContext};
use degenbot_simulation::BlockPriorityFees;
use degenbot_submission::{PoolKey, SubmitCandidate};
use pyo3::exceptions::PyValueError;
use pyo3::types::PyList;
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The signature re-exported so the `#[pyo3(signature)]` reference stays in
/// sync with the exposed type (mirrors the convention in `submit.rs`).
///
/// # Errors
///
/// `ValueError`: if `candidates` holds a non-`PyDispatchCandidate` element
/// (the GIL-held arg extraction rejects it before the async block is created),
/// or if the core `dispatch_profitable_results` returns a `DispatchError`
/// (an unrecoverable semaphore-acquisition failure — never happens in normal
/// operation).
///
/// # Panics
///
/// Panics if the dispatcher or suppression mutex is poisoned (a peer task
/// panicked while holding it). Cannot happen under normal operation; a poison
/// indicates a bug in a sibling task (the dispatcher/suppression mutexes are
/// only ever locked for short synchronous spans).
#[pyfunction]
#[pyo3(signature = (candidates, context, dispatcher, base_fee_next, current_block, min_profit_net, min_profit_margin_bps))]
#[allow(clippy::too_many_arguments)]
pub fn dispatch_profitable_py<'py>(
    py: Python<'py>,
    candidates: &Bound<'_, PyList>,
    context: &PySimulateContext,
    dispatcher: &PyDispatcher,
    base_fee_next: u128,
    current_block: u64,
    min_profit_net: u128,
    min_profit_margin_bps: u64,
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
    let provider: Arc<AlloyProvider> = Arc::clone(&context.provider);
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

    // ── GIL release across the per-path simulation fan-out ──
    future_into_py(py, async move {
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
            block_priority_fees,
        };
        let outcome: DispatchOutcome = dispatch_profitable_results(
            built,
            &ctx,
            &suppression_arc,
            current_block,
            min_profit_net,
            min_profit_margin_bps,
        )
        .await
        .map_err(|e: DispatchError| PyValueError::new_err(format!("{e:?}")))?;

        // ── Join survivors → SubmitCandidates (pure Rust — no GIL needed) ──
        // The cockpit chains dispatch_profitable_py → dispatch_and_submit_py
        // straight through PyDispatchOutcome.gas_profitable, so the join
        // produces exactly the field set dispatch_and_submit consumes.
        let joined: Vec<SubmitCandidate> = outcome
            .gas_profitable
            .iter()
            .map(|r| join_sim_result(r, &path_info_by_id, executor_address))
            .collect();

        Ok(PyDispatchOutcome::from_join(joined, &outcome))
    })
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
