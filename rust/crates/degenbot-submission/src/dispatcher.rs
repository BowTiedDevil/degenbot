//! Dispatcher nonce/pool/task coordination state (row N3 of the submission
//! scope `SHT6GE` — `port-now`).
//!
//! Port of `examples/eth_backrun_v2_v3_v4_rust.py` `Dispatcher` (L579–L678) +
//! the composed `PathSuppression` (L509–L578). Pure coordination state:
//! `HashSet`/`HashMap` containers + a single-element `Arc<Mutex<u64>>` for the
//! by-reference block clock (read by monitor tasks across the consumer/
//! monitor boundary without a per-access lock in Python — Rust uses the
//! `Mutex`). Hot per-tx; GIL-held today across claim/release; property-
//! testable.
//!
//! # Standalone-core constraint (ADR-005)
//!
//! A standalone `cargo add degenbot` consumer driving the dispatch loop
//! reaches this coordination state via the umbrella `pub use
//! degenbot_submission::dispatcher` and can claim nonces, reserve pools,
//! track submission tasks, advance the block clock, record fee history, and
//! apply path suppression — with **zero Python, zero `pyo3`**.
//!
//! # Parity (ADR-005 §4.2)
//!
//! The §4.2 oracle is the Python `Dispatcher`/`PathSuppression` shapes.
//! Property tests pin the behavioral parity:
//! - nonce dedup (claim never returns a pending nonce),
//! - pool mutual exclusion (`is_path_blocked` true iff any path pool overlaps
//!   pending ∪ committed),
//! - ring prune at `FEE_HISTORY_WINDOW` (oldest block popped on overflow),
//! - `PathSuppression` state transitions (suppress → retry → permanent
//!   un-suppress on success).
//!
//! The `CurrentBlock` clock is driven by `next_base_fee` (N1 leaf `JTLWA3` —
//! CONSUMED, no hard edge); `block_priority_fees` is later FED by
//! `eth_feeHistory` (`ZUZANP`) in the N6 dispatch orchestration — this module
//! owns only the ring data structure + prune.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::task::JoinSet;

/// Maximum number of per-block priority-fee percentile samples retained in
/// [`Dispatcher::block_priority_fees`] (matches `FEE_HISTORY_WINDOW = 10` in
/// the Python oracle). On overflow the oldest block (min key) is pruned.
pub const FEE_HISTORY_WINDOW: usize = 10;

/// `PathSuppression` consecutive-failure count threshold (matches
/// `PATH_SUPPRESS_THRESHOLD = 10` in the Python oracle).
pub const PATH_SUPPRESS_THRESHOLD: u32 = 10;

/// `PathSuppression` retry interval in blocks (matches
/// `PATH_SUPPRESS_RETRY_INTERVAL = 100` in the Python oracle). A suppressed
/// path is retried once per interval; a successful retry permanently un-
/// suppresses it.
pub const PATH_SUPPRESS_RETRY_INTERVAL: u64 = 100;

/// Maximum retained block-time samples in
/// [`Dispatcher::block_times`] (matches `deque(maxlen=60)` in the Python
/// oracle). The deque drops the oldest on overflow.
pub const BLOCK_TIMES_WINDOW: usize = 60;

/// A submission-keyed pool identifier (V2/V3 pool address or V4 `pool_id_hex`
/// — the Rust pool-key union the dispatcher mutual-excludes over). Newtype
/// over `String` so pool keys are not confused with arbitrary strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey(String);

impl PoolKey {
    /// Construct a pool key from its display string representation.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Borrow the inner pool-key string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PoolKey {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PoolKey {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for PoolKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The minimal view of a confirmed/expired in-flight transaction the
/// dispatcher needs to release its held coordination state (nonce + pools).
///
/// Matches the `SubmittedTx` fields consumed by the Python
/// `Dispatcher.release_tx(tx)` — `tx.nonce` + `set(tx.pools)` (the
/// `tx_hash`/`submission_block` fields are monitor-only and not dispatcher
/// state). Owned by the N6 submit-orchestration sibling; this crate consumes
/// only the release-coordination projection (ADR-005 — compose the producer's
/// record, don't duplicate it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedTx {
    /// The account nonce this tx was submitted with (held in
    /// `pending_nonces` until release).
    pub nonce: u64,
    /// The pool keys locked by this tx (held in `pending_pools` until
    /// release).
    pub pools: Vec<PoolKey>,
}

impl CommittedTx {
    /// Construct a committed-tx view from its coordination fields.
    #[must_use]
    pub fn new(nonce: u64, pools: Vec<PoolKey>) -> Self {
        Self { nonce, pools }
    }
}

// ============================================================================
// PathSuppression
// ============================================================================

/// Track per-path simulation failures and suppress consistently failing paths.
///
/// After a path fails simulation `PATH_SUPPRESS_THRESHOLD` consecutive times
/// it is suppressed — excluded from the simulation candidate list. Suppressed
/// paths are retried every `PATH_SUPPRESS_RETRY_INTERVAL` blocks; if a retry
/// succeeds the path is permanently un-suppressed.
///
/// Port of `examples/eth_backrun_v2_v3_v4_rust.py` `PathSuppression`
/// (L509–L578).
///
/// # Ownership (ADR-003 / ergo `LITQFF`)
///
/// Standalone — **not** composed into [`Dispatcher`]. It is owned behind its
/// own `Arc<Mutex<PathSuppression>>` (held by `PyDispatcher` alongside the
/// `Dispatcher` arc) so the simulation seam (`dispatch_profitable_results`)
/// can lock it directly without locking the `Dispatcher` (which the
/// submission monitor tasks contend for). The `Dispatcher` never touches
/// `PathSuppression` — the `record_success`/`record_failure`/`is_suppressed`/
/// `total_suppressed`/`discard_path` accessors live on `PyDispatcher` (the
/// `PyO3` seam), delegating to the suppression arc.
#[derive(Debug, Default)]
pub struct PathSuppression {
    /// `path_id` → consecutive failure count.
    fail_counts: BTreeMap<u64, u32>,
    /// `path_id` → block number when the path was last retried.
    last_retry_block: BTreeMap<u64, u64>,
    /// `path_id` set currently suppressed.
    suppressed: HashSet<u64>,
    /// Total paths suppressed (for logging parity with `total_suppressed`).
    total_suppressed: u64,
}

impl PathSuppression {
    /// Construct a fresh, empty suppression tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A path succeeded at simulation — reset its failure counter (and
    /// permanently un-suppress if it was suppressed).
    pub fn record_success(&mut self, path_id: u64) {
        self.fail_counts.remove(&path_id);
        if self.suppressed.remove(&path_id) {
            // matches the Python debug log path; no observable behavior.
        }
    }

    /// A path failed simulation — increment its counter, maybe suppress.
    pub fn record_failure(&mut self, path_id: u64) {
        let count = self.fail_counts.get(&path_id).copied().unwrap_or(0) + 1;
        self.fail_counts.insert(path_id, count);

        if count >= PATH_SUPPRESS_THRESHOLD && !self.suppressed.contains(&path_id) {
            self.suppressed.insert(path_id);
            self.total_suppressed += 1;
        }
    }

    /// Check if a path is currently suppressed (with retry logic).
    ///
    /// Returns `true` if the path should be skipped. Returns `false` if the
    /// path is due for a retry — the caller should simulate it (and the retry
    /// block is recorded so the next `is_suppressed` within the same interval
    /// suppresses).
    pub fn is_suppressed(&mut self, path_id: u64, current_block: u64) -> bool {
        if !self.suppressed.contains(&path_id) {
            return false;
        }

        let last_retry = self.last_retry_block.get(&path_id).copied().unwrap_or(0);
        if current_block.saturating_sub(last_retry) >= PATH_SUPPRESS_RETRY_INTERVAL {
            self.last_retry_block.insert(path_id, current_block);
            return false;
        }

        true
    }

    /// Total paths suppressed (cumulative — matches the Python
    /// `total_suppressed` property; never decremented).
    #[must_use]
    pub fn total_suppressed(&self) -> u64 {
        self.total_suppressed
    }

    /// Permanently discard suppression tracking for a de-registered path.
    pub fn discard(&mut self, path_id: u64) {
        self.fail_counts.remove(&path_id);
        self.suppressed.remove(&path_id);
        self.last_retry_block.remove(&path_id);
    }

    /// Whether `path_id` is in the suppressed set (read-only; does NOT apply
    /// retry logic — for inspection/logging parity only).
    #[must_use]
    pub fn is_in_suppressed_set(&self, path_id: u64) -> bool {
        self.suppressed.contains(&path_id)
    }
}

// ============================================================================
// Dispatcher
// ============================================================================

/// Coordination state for the dispatch loop.
///
/// Port of `examples/eth_backrun_v2_v3_v4_rust.py` `Dispatcher` (L579–L678).
/// Bundles the seven mutable containers `main()` previously declared as
/// exploded locals and threaded through `consume_result_batches`,
/// `dispatch_profitable_results`, and `monitor_pending_transaction`. The
/// containers are mutated in place by the methods. `PathSuppression` is
/// composed (and delegated) so all dispatch coordination lives behind one
/// object.
#[derive(Debug)]
pub struct Dispatcher {
    /// Nonces currently claimed by an in-flight tx (the free-pool exclusion
    /// set). `claim_nonce(start)` scans from `start` for the first non-member
    /// (matches the Python `next(n for n in itertools.count(start)...)`
    /// linear scan) and reserves it.
    pending_nonces: HashSet<u64>,
    /// Pool keys locked by an in-flight tx (mutual exclusion across the
    /// dispatch batch).
    pending_pools: HashSet<PoolKey>,
    /// Submission tasks tracked for lifecycle (auto-discard on completion via
    /// tokio `JoinSet`). Matches `set[asyncio.Task]` + `add_done_callback(
    /// discard)`; `JoinSet` reaps finished tasks on `try_join_next`/abort.
    active_tasks: JoinSet<()>,
    /// By-reference current-block clock — `Arc<Mutex<u64>>` mirrors the
    /// single-element mutable-by-reference Python list `current_block_ref:
    /// list[int]` so monitor tasks read the current block across the
    /// consumer/monitor boundary without a per-access Python lock.
    current_block: Arc<Mutex<u64>>,
    /// Ring buffer of per-block `(block, timestamp)` samples (oldest dropped
    /// on overflow — `deque(maxlen=60)` parity).
    block_times: VecDeque<(u64, u64)>,
    /// Per-block priority-fee percentile samples: `block → (percentile →
    /// fee)`. Pruned to `FEE_HISTORY_WINDOW` on insert (min key popped on
    /// overflow — matches Python).
    block_priority_fees: BTreeMap<u64, BTreeMap<u64, u128>>,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self {
            pending_nonces: HashSet::new(),
            pending_pools: HashSet::new(),
            active_tasks: JoinSet::new(),
            current_block: Arc::new(Mutex::new(0)),
            block_times: VecDeque::with_capacity(BLOCK_TIMES_WINDOW),
            block_priority_fees: BTreeMap::new(),
        }
    }
}

impl Dispatcher {
    /// Construct a fresh `Dispatcher` seeded at `current_block`.
    ///
    /// Matches the Python `for_block(current_block)` classmethod.
    #[must_use]
    pub fn for_block(current_block: u64) -> Self {
        Self {
            current_block: Arc::new(Mutex::new(current_block)),
            ..Self::default()
        }
    }

    // ── nonce coordination ───────────────────────────────────────────────

    /// Return the first int >= `start` not already pending, and reserve it.
    ///
    /// Linear scan upward from `start` skipping members of `pending_nonces`
    /// (matches `next(n for n in itertools.count(start) if n not in
    /// pending_nonces)`).
    pub fn claim_nonce(&mut self, start: u64) -> u64 {
        let mut nonce = start;
        while !self.pending_nonces.insert(nonce) {
            nonce += 1;
        }
        nonce
    }

    /// Release a nonce back to the free pool (tx confirmed or voided).
    pub fn release_nonce(&mut self, nonce: u64) {
        self.pending_nonces.remove(&nonce);
    }

    // ── pool mutual exclusion ────────────────────────────────────────────

    /// Mark pool keys as locked by an in-flight tx.
    pub fn reserve_pools<I>(&mut self, path_pools: I)
    where
        I: IntoIterator<Item = PoolKey>,
    {
        self.pending_pools.extend(path_pools);
    }

    /// Release pool keys when a tx confirms or expires.
    pub fn release_pools<I>(&mut self, pools: I)
    where
        I: IntoIterator<Item = PoolKey>,
    {
        for p in pools {
            self.pending_pools.remove(&p);
        }
    }

    /// True iff any path pool is already locked (pending) or committed this
    /// batch. Matches `bool(path_pools & (pending_pools | committed_pools))`.
    #[must_use]
    pub fn is_path_blocked(
        &self,
        path_pools: &HashSet<PoolKey>,
        committed_pools: &HashSet<PoolKey>,
    ) -> bool {
        if path_pools.is_empty() {
            return false;
        }
        // Fast path: the smaller of pending/committed is scanned in the outer
        // loop against the path pool set.
        path_pools
            .iter()
            .any(|p| self.pending_pools.contains(p) || committed_pools.contains(p))
    }

    /// Release both the nonce and pools held by a completed/expired tx.
    pub fn release_tx(&mut self, tx: &CommittedTx) {
        self.release_nonce(tx.nonce);
        let pools: Vec<PoolKey> = tx.pools.clone();
        self.release_pools(pools);
    }

    // ── task tracking ────────────────────────────────────────────────────

    /// Register an in-flight submission task; auto-discard on completion.
    ///
    /// Spawns `task` onto the internal `JoinSet` (the dispatcher is the spawn
    /// point for tracked submission tasks — the N6 dispatch orchestration
    /// feeds futures in). Matches Python `track_task(task)` + the
    /// `add_done_callback(discard)` auto-removal; `JoinSet` reaps finished
    /// tasks on [`Self::reap_finished`] / abort.
    #[allow(clippy::future_not_send)] // task may be !Send (per-dispatch local)
    pub fn track_task<F>(&mut self, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.active_tasks.spawn(task);
    }

    /// Number of currently-tracked tasks (pending + finished-not-yet-reaped).
    #[must_use]
    pub fn active_task_count(&self) -> usize {
        self.active_tasks.len()
    }

    /// Reap finished tasks (auto-discard parity). Polls once per ready task;
    /// returns the number reaped. The N6 dispatch loop calls this on its
    /// tick.
    pub fn reap_finished(&mut self) -> usize {
        let mut reaped = 0;
        while self.active_tasks.try_join_next().is_some() {
            reaped += 1;
        }
        reaped
    }

    /// Abort all tracked tasks (shutdown parity).
    pub fn abort_all_tasks(&mut self) {
        self.active_tasks.abort_all();
    }

    // ── block / fee recording ───────────────────────────────────────────

    /// The current block (read by monitor tasks by reference).
    ///
    /// # Panics
    /// Panics if the internal `Mutex` is poisoned (a monitor task panicked
    /// while holding it — unrecoverable; matches the Python assumption that
    /// the single-element list is always readable).
    #[must_use]
    pub fn current_block(&self) -> u64 {
        *self
            .current_block
            .lock()
            .expect("current_block mutex poisoned")
    }

    /// Update the current block (read by monitor tasks by reference).
    ///
    /// # Panics
    /// Panics if the internal `Mutex` is poisoned.
    pub fn advance_block(&self, block: u64) {
        *self
            .current_block
            .lock()
            .expect("current_block mutex poisoned") = block;
    }

    /// Get a clone of the by-reference block handle (so monitor tasks can
    /// read the clock across the consumer/monitor boundary without going
    /// through the dispatcher). Matches the Python `current_block_ref` list
    /// being passed by reference.
    #[must_use]
    pub fn current_block_handle(&self) -> Arc<Mutex<u64>> {
        Arc::clone(&self.current_block)
    }

    /// Record a `(block, timestamp)` sample, dropping the oldest on overflow
    /// (`deque(maxlen=BLOCK_TIMES_WINDOW)` parity).
    pub fn record_block_time(&mut self, block: u64, timestamp: u64) {
        if self.block_times.len() >= BLOCK_TIMES_WINDOW {
            self.block_times.pop_front();
        }
        self.block_times.push_back((block, timestamp));
    }

    /// Record per-block percentile fees, pruning to `FEE_HISTORY_WINDOW`.
    /// On overflow the min key (oldest block) is popped (matches Python
    /// `block_priority_fees.pop(min(...))`).
    pub fn record_priority_fees(&mut self, block: u64, fees: BTreeMap<u64, u128>) {
        self.block_priority_fees.insert(block, fees);
        if self.block_priority_fees.len() > FEE_HISTORY_WINDOW {
            // BTreeMap keeps keys sorted; pop_first removes the oldest block.
            self.block_priority_fees.pop_first();
        }
    }

    /// Return the most-recently recorded per-block percentile fees (the max
    /// block key). Matches Python `block_priority_fees[max(...)]`.
    ///
    /// # Panics
    /// Panics if no fees have been recorded yet (matches the Python
    /// `max([])` `ValueError` on an empty dict — the caller must record
    /// before reading; this is a programming error, not a recoverable
    /// condition).
    #[must_use]
    pub fn latest_priority_fees(&self) -> &BTreeMap<u64, u128> {
        self.block_priority_fees
            .last_key_value()
            .map(|(_, v)| v)
            .expect("latest_priority_fees called before any record_priority_fees")
    }

    /// Borrow the full per-block priority-fee ring (the
    /// `block -> {percentile: fee}` map). Used by the `PyO3` seam so the
    /// Python `_compute_priority_fee` (which reads `max(ring)` for the
    /// latest block's percentiles) works unchanged — returns an empty
    /// map when nothing has been recorded yet (matching the Python
    /// `dispatcher.block_priority_fees` empty-dict behavior — no panic).
    #[must_use]
    pub fn block_priority_fees(&self) -> &BTreeMap<u64, BTreeMap<u64, u128>> {
        &self.block_priority_fees
    }

    // ── PathSuppression ───────────────────────────────────────────────────
    // (Removed — LITQFF) `PathSuppression` is no longer composed into
    // `Dispatcher`. The `record_success`/`record_failure`/`is_suppressed`/
    // `total_suppressed`/`discard_path` accessors live on `PyDispatcher` (the
    // PyO3 seam), delegating to the standalone `Arc<Mutex<PathSuppression>>`.
    // The simulation seam takes that arc directly; `Dispatcher` never touches
    // suppression. This keeps the `Dispatcher` lock uncontended during the
    // simulation fan-out (the submission monitor tasks that contend for the
    // `Dispatcher` lock never block on suppression bookkeeping).

    // ── inspection (test + monitor parity) ──────────────────────────────

    /// Number of currently-pending nonces.
    #[must_use]
    pub fn pending_nonce_count(&self) -> usize {
        self.pending_nonces.len()
    }

    /// Number of currently-pending (locked) pools.
    #[must_use]
    pub fn pending_pool_count(&self) -> usize {
        self.pending_pools.len()
    }

    /// Number of recorded block-time samples.
    #[must_use]
    pub fn block_time_count(&self) -> usize {
        self.block_times.len()
    }

    /// Iterate recorded block-time samples (oldest first).
    pub fn block_times(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.block_times.iter().copied()
    }

    /// Whether `pool` is currently pending (locked).
    #[must_use]
    pub fn is_pool_pending(&self, pool: &PoolKey) -> bool {
        self.pending_pools.contains(pool)
    }
}

#[cfg(test)]
#[allow(clippy::too_many_lines)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // §4.2 property: claim_nonce never returns a pending nonce, and the
    // returned nonce is immediately reserved (a subsequent claim from the
    // same start skips it).
    #[test]
    fn claim_nonce_reserves_and_dedups() {
        let mut d = Dispatcher::default();
        let n0 = d.claim_nonce(10);
        assert_eq!(n0, 10);
        // same start skips the now-pending 10
        let n1 = d.claim_nonce(10);
        assert_eq!(n1, 11);
        // explicit release frees 10 again
        d.release_nonce(10);
        let n2 = d.claim_nonce(10);
        assert_eq!(n2, 10);
        assert_eq!(d.pending_nonce_count(), 2); // 10 + 11
    }

    proptest! {
        /// Claiming `k` nonces from `start` over a set with some pre-seeded pending
        /// nonces always yields distinct values not in the pending set, and each is
        /// reserved afterward.
        #[test]
        fn prop_claim_nonce_dedup(
            start in 0u64..1_000,
            seeds in proptest::collection::hash_set(0u64..2_000, 0..50),
            k in 1usize..100,
        ) {
            let mut d = Dispatcher::default();
            for s in &seeds {
                d.pending_nonces_raw_insert(*s);
            }
            let mut seen = HashSet::new();
            for _ in 0..k {
                let n = d.claim_nonce(start);
                proptest::prop_assert!(!seeds.contains(&n), "claimed a pending nonce");
                proptest::prop_assert!(seen.insert(n), "claimed a duplicate nonce");
            }
        }

        /// `is_path_blocked` is true iff at least one path pool overlaps pending ∪
        /// committed; empty path pool sets are never blocked.
        #[test]
        fn prop_is_path_blocked(
            path in proptest::collection::hash_set(0u64..100, 0..20),
            pending in proptest::collection::hash_set(0u64..100, 0..20),
            committed in proptest::collection::hash_set(0u64..100, 0..20),
        ) {
            let mut d = Dispatcher::default();
            for p in &pending {
                d.reserve_pools(vec![PoolKey::new(p.to_string())]);
            }
            let committed_set: HashSet<PoolKey> = committed.iter()
                .map(|p| PoolKey::new(p.to_string())).collect();
            let path_set: HashSet<PoolKey> = path.iter()
                .map(|p| PoolKey::new(p.to_string())).collect();
            let blocked = d.is_path_blocked(&path_set, &committed_set);
            // blocked iff path ∩ (pending ∪ committed) is non-empty
            let union: HashSet<u64> = pending.union(&committed).copied().collect();
            let overlap: HashSet<u64> = path.intersection(&union).copied().collect();
            if path.is_empty() {
                proptest::prop_assert!(!blocked);
            } else {
                proptest::prop_assert_eq!(blocked, !overlap.is_empty());
            }
        }

        /// Recording `FEE_HISTORY_WINDOW + n` blocks keeps the ring pruned to
        /// exactly `FEE_HISTORY_WINDOW`, and `latest_priority_fees` reflects the
        /// max (most-recent) block.
        #[test]
        fn prop_priority_fee_ring_prune(
            extra in 0u64..50,
        ) {
            let mut d = Dispatcher::default();
            for block in 0..(FEE_HISTORY_WINDOW as u64 + extra) {
                let mut fees = BTreeMap::new();
                fees.insert(50, u128::from(block) * 2);
                d.record_priority_fees(100 + block, fees);
            }
            let recorded = FEE_HISTORY_WINDOW as u64 + extra;
            let expected_len =
                usize::try_from(recorded).unwrap_or(usize::MAX).min(FEE_HISTORY_WINDOW);
            proptest::prop_assert_eq!(d.block_priority_fees_len(), expected_len);
            // latest is the most-recent block recorded
            if recorded > 0 {
                let latest = d.latest_priority_fees();
                proptest::prop_assert_eq!(
                    *latest.get(&50).unwrap(),
                    u128::from(recorded - 1) * 2
                );
            }
        }

        /// `PathSuppression`: a path suppressed after `THRESHOLD` consecutive
        /// failures; retry after `RETRY_INTERVAL` blocks un-suppresses for one
        /// poll; a `record_success` permanently un-suppresses.
        #[test]
        fn prop_suppression_state_machine(
            path_id in 0u64..10,
        ) {
            let mut s = PathSuppression::new();
            // not suppressed initially
            proptest::prop_assert!(!s.is_suppressed(path_id, 0));
            // record THRESHOLD-1 failures: still not suppressed
            for _ in 0..(PATH_SUPPRESS_THRESHOLD - 1) {
                s.record_failure(path_id);
            }
            proptest::prop_assert!(!s.is_suppressed(path_id, 0));
            // one more failure: suppressed
            s.record_failure(path_id);
            proptest::prop_assert!(s.is_suppressed(path_id, 0));
            proptest::prop_assert_eq!(s.total_suppressed(), 1);
            // within retry interval: stays suppressed
            proptest::prop_assert!(s.is_suppressed(path_id, PATH_SUPPRESS_RETRY_INTERVAL - 1));
            // at retry interval: allowed once (returns false) then re-suppressed
            proptest::prop_assert!(!s.is_suppressed(path_id, PATH_SUPPRESS_RETRY_INTERVAL));
            proptest::prop_assert!(s.is_suppressed(path_id, PATH_SUPPRESS_RETRY_INTERVAL));
            // record_success permanently un-suppresses
            s.record_success(path_id);
            proptest::prop_assert!(!s.is_suppressed(path_id, PATH_SUPPRESS_RETRY_INTERVAL * 2));
        }
    }

    #[test]
    fn block_times_deque_prunes_to_window() {
        let mut d = Dispatcher::default();
        for i in 0..u64::try_from(BLOCK_TIMES_WINDOW + 5).unwrap() {
            d.record_block_time(i, i * 12);
        }
        assert_eq!(d.block_time_count(), BLOCK_TIMES_WINDOW);
        // oldest is the first surviving sample
        let (oldest_block, _) = d.block_times().next().unwrap();
        assert_eq!(oldest_block, 5);
    }

    #[test]
    fn current_block_ref_shared_across_handle() {
        let d = Dispatcher::for_block(42);
        let handle = d.current_block_handle();
        d.advance_block(100);
        // monitor-task simulation: read via the cloned handle
        assert_eq!(*handle.lock().unwrap(), 100);
        assert_eq!(d.current_block(), 100);
    }

    #[test]
    fn release_tx_releases_nonce_and_pools() {
        let mut d = Dispatcher::default();
        d.reserve_pools(vec![PoolKey::new("poolA"), PoolKey::new("poolB")]);
        let _ = d.claim_nonce(7);
        assert!(d.is_pool_pending(&PoolKey::new("poolA")));
        assert_eq!(d.pending_nonce_count(), 1);

        let tx = CommittedTx::new(7, vec![PoolKey::new("poolA"), PoolKey::new("poolB")]);
        d.release_tx(&tx);
        assert!(!d.is_pool_pending(&PoolKey::new("poolA")));
        assert_eq!(d.pending_nonce_count(), 0);
    }

    #[test]
    fn for_block_seeds_current_block() {
        let d = Dispatcher::for_block(12345);
        assert_eq!(d.current_block(), 12345);
    }

    #[test]
    fn discard_path_clears_all_tracking() {
        let mut s = PathSuppression::new();
        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            s.record_failure(5);
        }
        assert!(s.is_suppressed(5, 0));
        s.discard(5);
        assert!(!s.is_suppressed(5, 0));
        assert_eq!(s.total_suppressed(), 1); // cumulative, not decremented
    }

    #[tokio::test]
    async fn tracked_tasks_auto_discard_on_completion() {
        let mut d = Dispatcher::for_block(1);
        assert_eq!(d.active_task_count(), 0);
        d.track_task(async {});
        d.track_task(async {});
        assert_eq!(d.active_task_count(), 2);
        // let them run to completion
        tokio::task::yield_now().await;
        let reaped = d.reap_finished();
        assert_eq!(reaped, 2);
        assert_eq!(d.active_task_count(), 0);
    }

    #[tokio::test]
    #[allow(clippy::duration_suboptimal_units)] // std has no min-unit
    async fn abort_all_tasks_cancels_pending() {
        let mut d = Dispatcher::for_block(1);
        d.track_task(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        assert_eq!(d.active_task_count(), 1);
        d.abort_all_tasks();
        // the runtime must poll to process the abort signal before reaping
        tokio::task::yield_now().await;
        let _ = d.reap_finished();
        assert_eq!(d.active_task_count(), 0);
    }

    // ── test-only seam to seed pending nonces for property tests ──────────
    impl Dispatcher {
        fn pending_nonces_raw_insert(&mut self, n: u64) {
            self.pending_nonces.insert(n);
        }
        fn block_priority_fees_len(&self) -> usize {
            self.block_priority_fees.len()
        }
    }
}
