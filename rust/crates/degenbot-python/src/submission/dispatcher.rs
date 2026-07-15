//! `PyDispatcher` — `PyO3` wrapper over [`degenbot_submission::Dispatcher`].
//!
//! The coordination-state value object for the dispatch loop. Python held a
//! separate mirror (`examples/eth_backrun_v2_v3_v4_rust.py` `class Dispatcher`)
//! duplicating the Rust-owned [`Dispatcher`] (`M756BN` + `5FB5MW`
//! `PathSuppression`). This wrapper makes the Rust-owned state the single
//! source — Python constructs + drives it via the `PyO3` seam (ADR-005 §3).
//!
//! The held [`Dispatcher`] is shared behind `Arc<Mutex<Dispatcher>>` so the
//! submit-orchestration leaf (`dispatch_and_submit`) + the monitor tasks + the
//! block-clock reader all see one state. The Python wrapper holds the SAME
//! `Arc` the Rust leaves lock — no Python-side mirror.
//!
//! `PathSuppression` is **not** composed into [`Dispatcher`] (ergo `LITQFF`):
//! it lives behind its own `Arc<Mutex<PathSuppression>>`, held here alongside
//! the `Dispatcher` arc, so the simulation seam (`dispatch_profitable_py`)
//! can lock suppression WITHOUT locking the `Dispatcher` (which the
//! submission monitor tasks contend for). The `Dispatcher` never touches
//! suppression; the 5 suppression accessors below delegate to the suppression
//! arc.
//!
//! # GIL discipline
//!
//! Every method is a short synchronous coordination op (nonce/pool bookkeeping,
//! a block-clock advance, a fee-history ring append) — the lock is held for
//! sub-µs compute with no `.await`. No GIL release is needed (the Rust lock
//! does not depend on the GIL; the ops are trivial). The async submit leaf that
//! DOES release the GIL across RPC is `dispatch_and_submit_py` (separate file).

use crate::prelude::*;
use degenbot_submission::{CommittedTx, Dispatcher, PathSuppression, PoolKey};
use pyo3::types::PyTuple;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The coordination-state value object for the dispatch loop.
///
/// Bundles the mutable containers `consume_result_batches` /
/// `dispatch_profitable_results` / `monitor_pending_transaction` previously
/// held as a Python mirror: pending nonces/pools, in-flight task count, the
/// current-block reference, the block-time ring, per-block priority fees.
/// `PathSuppression` is **not** composed (ergo `LITQFF`); it lives behind its
/// own `Arc<Mutex<PathSuppression>>` on `PyDispatcher` so the simulation seam
/// locks it directly, never the `Dispatcher`.
///
/// Construct via [`PyDispatcher::for_block`]. The held [`Dispatcher`] is owned
/// by Rust; Python drives it through the `PyO3` seam (the Rust leaves
/// `dispatch_and_submit` / `fetch_fee_history` / `monitor_pending_transaction`
/// receive the SAME `Arc<Mutex<Dispatcher>>` + lock it themselves).
#[pyclass(name = "PyDispatcher", skip_from_py_object, module = "degenbot._ffi")]
pub struct PyDispatcher {
    pub(crate) inner: Arc<Mutex<Dispatcher>>,
    /// The standalone path-suppression tracker (LITQFF) — held alongside the
    /// `Dispatcher` arc so the simulation seam (`dispatch_profitable_py`)
    /// can lock suppression WITHOUT locking the `Dispatcher` (the submission
    /// monitor tasks contend for the `Dispatcher` lock; suppression is
    /// touched only at the sim fan-out bookends, dormant across the RPC
    /// `.await`s, and uncontended during the fan-out).
    pub(crate) suppression: Arc<Mutex<PathSuppression>>,
}

impl PyDispatcher {
    /// Borrow the shared `Arc<Mutex<Dispatcher>>` (for the submit/fee/monitor
    /// leaves that take it by reference).
    pub(crate) fn inner_arc(&self) -> Arc<Mutex<Dispatcher>> {
        Arc::clone(&self.inner)
    }

    /// Borrow the shared `Arc<Mutex<PathSuppression>>` (for the simulation
    /// seam's `dispatch_profitable_py` — A4/QQFTB4 — which locks suppression
    /// for the `dispatch_profitable_results` call; the `Dispatcher` arc is
    /// NOT touched, so monitor-task contention on the `Dispatcher` is
    /// unaffected).
    #[allow(dead_code)]
    pub(crate) fn suppression_arc(&self) -> Arc<Mutex<PathSuppression>> {
        Arc::clone(&self.suppression)
    }
}

#[pymethods]
impl PyDispatcher {
    /// Construct a fresh dispatcher seeded at `current_block`.
    ///
    /// Args:
    ///     `current_block`: The block to seed the by-reference block clock with.
    #[staticmethod]
    fn for_block(current_block: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Dispatcher::for_block(current_block))),
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
        }
    }

    // ── nonce coordination ────────────────────────────────────────
    /// Return the first `int >= start` not already pending, and reserve it.
    fn claim_nonce(&self, start: u64) -> u64 {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .claim_nonce(start)
    }

    /// Release a nonce back to the free pool (tx confirmed or voided).
    fn release_nonce(&self, nonce: u64) {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .release_nonce(nonce);
    }

    // ── pool mutual exclusion ────────────────────────────────────
    /// Mark Rust pool keys as locked by an in-flight tx.
    fn reserve_pools(&self, path_pools: HashSet<String>) {
        let keys: Vec<PoolKey> = path_pools.into_iter().map(PoolKey::from).collect();
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .reserve_pools(keys);
    }

    /// Release Rust pool keys when a tx confirms or expires.
    fn release_pools(&self, pools: HashSet<String>) {
        let keys: Vec<PoolKey> = pools.into_iter().map(PoolKey::from).collect();
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .release_pools(keys);
    }

    /// True iff any path pool is already locked (pending) or committed this batch.
    fn is_path_blocked(
        &self,
        path_pools: HashSet<String>,
        committed_pools: HashSet<String>,
    ) -> bool {
        let path: HashSet<PoolKey> = path_pools.into_iter().map(PoolKey::from).collect();
        let committed: HashSet<PoolKey> = committed_pools.into_iter().map(PoolKey::from).collect();
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .is_path_blocked(&path, &committed)
    }

    /// Release both the nonce and pools held by a completed/expired tx.
    ///
    /// Args:
    ///     `tx`: A `(nonce, pools)` tuple — the minimal committed-tx view.
    ///              (`PySubmittedTx` round-trips to this; the dispatch
    ///              orchestration builds it from the Rust `CommittedTx`.)
    fn release_tx(&self, tx: &Bound<'_, PyTuple>) -> PyResult<()> {
        let nonce: u64 = tx.get_item(0)?.extract()?;
        let pools: HashSet<String> = tx.get_item(1)?.extract()?;
        let committed = CommittedTx::new(nonce, pools.into_iter().map(PoolKey::from).collect());
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .release_tx(&committed);
        Ok(())
    }

    // ── task tracking ─────────────────────────────────────────────
    /// Count of in-flight spawned monitor/broadcast tasks (best-effort,
    /// reaped of finished entries).
    fn active_task_count(&self) -> usize {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .active_task_count()
    }

    /// Reap finished tasks; return the count reaped.
    fn reap_finished(&self) -> usize {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .reap_finished()
    }

    /// Abort all tracked tasks (cold-shutdown).
    fn abort_all_tasks(&self) {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .abort_all_tasks();
    }

    // ── block / fee recording ─────────────────────────────────────
    /// The current block (the by-reference block clock).
    #[getter]
    fn current_block(&self) -> u64 {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .current_block()
    }

    /// Update the current block (read by monitor tasks by reference).
    fn advance_block(&self, block: u64) {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .advance_block(block);
    }

    /// Record a `(block, timestamp)` pair into the block-time ring (maxlen 60).
    fn record_block_time(&self, block: u64, timestamp: u64) {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .record_block_time(block, timestamp);
    }

    /// Record per-block percentile fees, pruning to the fee-history window.
    ///
    /// Args:
    ///     `block`: The block the fees are for.
    ///     `fees`: A `{percentile: wei}` dict (e.g. `{50: 1_000_000_000}`).
    fn record_priority_fees(&self, block: u64, fees: std::collections::BTreeMap<u64, u128>) {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .record_priority_fees(block, fees);
    }

    /// The most-recently recorded per-block percentile fees (`{p: wei}`).
    ///
    /// Raises:
    ///     `ValueError`: If no fees have been recorded yet (the ring is empty).
    fn latest_priority_fees(&self) -> PyResult<std::collections::BTreeMap<u64, u128>> {
        let guard = self.inner.lock().expect("dispatcher mutex poisoned");
        let fees = guard.latest_priority_fees();
        if fees.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "dispatcher priority-fee ring is empty",
            ));
        }
        Ok(fees.clone())
    }

    /// The full per-block priority-fee ring (`{block: {percentile: wei}}`),
    /// empty when nothing recorded. Read by `_compute_priority_fee` (which
    /// does `max(ring)` for the latest block's percentiles) — preserves the
    /// Python `dispatcher.block_priority_fees` attribute shape unchanged.
    #[getter]
    fn block_priority_fees(
        &self,
    ) -> std::collections::BTreeMap<u64, std::collections::BTreeMap<u64, u128>> {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .block_priority_fees()
            .clone()
    }

    // ── block-time ring access (the [block:] log latency read) ────
    /// Count of recorded block-time pairs (capped at the ring maxlen).
    fn block_time_count(&self) -> usize {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .block_time_count()
    }

    /// The oldest `(block, timestamp)` pair in the ring (the latency baseline).
    ///
    /// Raises:
    ///     `IndexError`: If the ring is empty.
    fn block_times_oldest(&self) -> PyResult<(u64, u64)> {
        let guard = self.inner.lock().expect("dispatcher mutex poisoned");
        let mut iter = guard.block_times();
        match iter.next() {
            Some(bt) => Ok(bt),
            None => Err(pyo3::exceptions::PyIndexError::new_err(
                "dispatcher block-time ring is empty",
            )),
        }
    }

    // ── PathSuppression delegation ────────────────────────────────
    // Delegated to the standalone `Arc<Mutex<PathSuppression>>` (LITQFF), NOT
    // the `Dispatcher` arc — the sim seam locks the same arc directly, and the
    // `Dispatcher` lock stays uncontended by suppression bookkeeping.
    /// Record a successful simulation for `path_id` (clears its fail streak).
    fn record_success(&self, path_id: u64) {
        self.suppression
            .lock()
            .expect("suppression mutex poisoned")
            .record_success(path_id);
    }

    /// Record a failed simulation for `path_id` (bumps its fail streak;
    /// suppresses after `PATH_SUPPRESS_THRESHOLD` consecutive failures).
    fn record_failure(&self, path_id: u64) {
        self.suppression
            .lock()
            .expect("suppression mutex poisoned")
            .record_failure(path_id);
    }

    /// True iff `path_id` is currently suppressed (retried every
    /// `PATH_SUPPRESS_RETRY_INTERVAL` blocks).
    fn is_suppressed(&self, path_id: u64, current_block: u64) -> bool {
        self.suppression
            .lock()
            .expect("suppression mutex poisoned")
            .is_suppressed(path_id, current_block)
    }

    /// Count of paths currently in the suppressed set.
    fn total_suppressed(&self) -> u64 {
        self.suppression
            .lock()
            .expect("suppression mutex poisoned")
            .total_suppressed()
    }

    /// Discard a path from suppression tracking (the result-batch `removed` set).
    fn discard_path(&self, path_id: u64) {
        self.suppression
            .lock()
            .expect("suppression mutex poisoned")
            .discard(path_id);
    }

    // ── introspection (the [dispatch]/[sim] summary reads) ────────
    /// Count of nonces claimed but not yet released.
    fn pending_nonce_count(&self) -> usize {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .pending_nonce_count()
    }

    /// Count of pool keys reserved but not yet released.
    fn pending_pool_count(&self) -> usize {
        self.inner
            .lock()
            .expect("dispatcher mutex poisoned")
            .pending_pool_count()
    }
}
