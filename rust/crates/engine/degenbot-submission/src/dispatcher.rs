//! Dispatcher nonce/pool/task coordination state.
//!
//! The dispatcher's coordination state, composing `PathSuppression`. Pure state:
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
//! degenbot_submission::dispatcher` and can reserve pools, track submission
//! tasks, advance the block clock, record fee history, and apply path
//! suppression — with **zero Python, zero `pyo3`**.
//!
//! Nonce coordination does **not** live here. Issuance and release are owned
//! by the process-wide `degenbot_substrate::nonce::NonceAuthority`, and a
//! sign path obtains its nonce from the strategy's `NonceLane`. The dispatcher
//! coordinates the pool mutual-exclusion set and the monitor task set only.
//!
//! # Parity gate (`docs/architecture/parity-gate.md`)
//!
//! The parity oracle is the Python `Dispatcher`/`PathSuppression` shapes.
//! Property tests pin the behavioral parity:
//! - pool mutual exclusion (`is_path_blocked` true iff any path pool overlaps
//!   pending ∪ committed),
//! - ring prune at `FEE_HISTORY_WINDOW` (oldest block popped on overflow),
//! - `PathSuppression` state transitions (suppress → retry → permanent
//!   un-suppress on success).
//!
//! The `CurrentBlock` clock is driven by `next_base_fee` (CONSUMED, no hard
//! edge); `block_priority_fees` is later FED by `eth_feeHistory` in the
//! dispatch orchestration — this module owns only the ring data structure +
//! prune.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use tokio::task::JoinSet;

use degenbot_execution::CandidateId;

/// Maximum number of per-block priority-fee percentile samples retained in
/// [`Dispatcher::block_priority_fees`]: ten blocks of `eth_feeHistory` — the
/// window the p10/p50 fee bounds are drawn from. On overflow the oldest block
/// (min key) is pruned.
pub const FEE_HISTORY_WINDOW: usize = 10;

/// `PathSuppression` consecutive-failure count threshold: a path that fails
/// ten times in a row is suppressed.
pub const PATH_SUPPRESS_THRESHOLD: u32 = 10;

/// `PathSuppression` retry interval in blocks: a suppressed path is retried
/// once per 100 blocks; a successful retry permanently un-suppresses it.
pub const PATH_SUPPRESS_RETRY_INTERVAL: u64 = 100;

/// Maximum retained block-time samples in [`Dispatcher::block_times`]. The
/// window drops the oldest on overflow, so the block-time average tracks the
/// recent chain rather than its whole history.
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
/// dispatcher needs to release its held pool coordination state.
///
/// Matches the `SubmittedTx` fields consumed by the Python
/// `Dispatcher.release_tx(tx)` — `set(tx.pools)` (the `tx_hash`/`nonce`/
/// `submission_block` fields are monitor-only and not dispatcher state).
/// Nonce release belongs to the `NonceAuthority`, not the dispatcher. Owned by
/// the N6 submit-orchestration sibling; this crate consumes only the
/// release-coordination projection (ADR-005 — compose the producer's record,
/// don't duplicate it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedTx {
    /// The pool keys locked by this tx (held in `pending_pools` until
    /// release).
    pub pools: Vec<PoolKey>,
}

impl CommittedTx {
    /// Construct a committed-tx view from its pool coordination fields.
    #[must_use]
    pub fn new(pools: Vec<PoolKey>) -> Self {
        Self { pools }
    }
}

// ============================================================================
// PathSuppression
// ============================================================================

/// Track per-candidate simulation failures and suppress consistently failing
/// candidates.
///
/// After a candidate fails simulation `PATH_SUPPRESS_THRESHOLD` consecutive
/// times it is suppressed — excluded from the simulation candidate list.
/// Suppressed candidates are retried every `PATH_SUPPRESS_RETRY_INTERVAL`
/// blocks; if a retry succeeds the candidate is permanently un-suppressed.
///
/// The key is the [`CandidateId`], not the solver path id: one path evaluated
/// at two deployments is two candidates, and a failure at one deployment says
/// nothing about the other (ADR-098 D4). A path leaving the registry is the one
/// path-scoped event, handled by [`PathSuppression::discard_path`].
///
/// The path-suppression ledger: which candidates are benched, and until when.
///
/// # Ownership
///
/// Standalone — **not** composed into [`Dispatcher`]. It is owned behind its
/// own `Arc<Mutex<PathSuppression>>` (held by `PyDispatcher` alongside the
/// `Dispatcher` arc) so the simulation seam (`dispatch_profitable_results`)
/// can lock it directly without locking the `Dispatcher` (which the
/// submission monitor tasks contend for). The `Dispatcher` never touches
/// `PathSuppression`: the ledger's mutators are the pipeline's own (the sim
/// fan-out records outcomes, the assembly reads the skip projection, and the
/// session discards a removed path's entries), and only the cumulative tally
/// crosses the `PyO3` seam for a driver to render.
#[derive(Debug, Default)]
pub struct PathSuppression {
    /// `candidate_id` → consecutive failure count.
    fail_counts: BTreeMap<CandidateId, u32>,
    /// `candidate_id` → block number when the candidate was last retried.
    last_retry_block: BTreeMap<CandidateId, u64>,
    /// `candidate_id` set currently suppressed.
    suppressed: HashSet<CandidateId>,
    /// Total paths suppressed (for logging parity with `total_suppressed`).
    total_suppressed: u64,
}

impl PathSuppression {
    /// Construct a fresh, empty suppression tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A candidate succeeded at simulation — reset its failure counter (and
    /// permanently un-suppress if it was suppressed).
    pub fn record_success(&mut self, candidate_id: CandidateId) {
        self.fail_counts.remove(&candidate_id);
        if self.suppressed.remove(&candidate_id) {
            // matches the Python debug log path; no observable behavior.
        }
    }

    /// A candidate failed simulation — increment its counter, maybe suppress.
    pub fn record_failure(&mut self, candidate_id: CandidateId) {
        let count = self.fail_counts.get(&candidate_id).copied().unwrap_or(0) + 1;
        self.fail_counts.insert(candidate_id, count);

        if count >= PATH_SUPPRESS_THRESHOLD && !self.suppressed.contains(&candidate_id) {
            self.suppressed.insert(candidate_id);
            self.total_suppressed += 1;
        }
    }

    /// Check if a candidate is currently suppressed (with retry logic).
    ///
    /// Returns `true` if the candidate should be skipped. Returns `false` if it
    /// is due for a retry — the caller should simulate it (and the retry block
    /// is recorded so the next `is_suppressed` within the same interval
    /// suppresses).
    pub fn is_suppressed(&mut self, candidate_id: CandidateId, current_block: u64) -> bool {
        if !self.suppressed.contains(&candidate_id) {
            return false;
        }

        let last_retry = self
            .last_retry_block
            .get(&candidate_id)
            .copied()
            .unwrap_or(0);
        if current_block.saturating_sub(last_retry) >= PATH_SUPPRESS_RETRY_INTERVAL {
            self.last_retry_block.insert(candidate_id, current_block);
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

    /// Permanently discard suppression tracking for one candidate.
    pub fn discard(&mut self, candidate_id: CandidateId) {
        self.fail_counts.remove(&candidate_id);
        self.suppressed.remove(&candidate_id);
        self.last_retry_block.remove(&candidate_id);
    }

    /// Permanently discard every candidate's tracking for a de-registered path.
    ///
    /// The engine registers and removes PATHS, not candidates: a path leaving
    /// the registry retires the path at every deployment that could have
    /// executed it.
    pub fn discard_path(&mut self, path_id: u64) {
        self.fail_counts.retain(|id, _| id.path_id() != path_id);
        self.suppressed.retain(|id| id.path_id() != path_id);
        self.last_retry_block
            .retain(|id, _| id.path_id() != path_id);
    }

    /// Whether `candidate_id` is in the suppressed set (read-only; does NOT
    /// apply retry logic — for inspection/logging parity only).
    #[must_use]
    pub fn is_in_suppressed_set(&self, candidate_id: CandidateId) -> bool {
        self.suppressed.contains(&candidate_id)
    }

    /// Whether the candidate is suppressed AND not due for a retry this block —
    /// the read-only projection of [`Self::is_suppressed`] WITHOUT the retry
    /// stamp.
    ///
    /// The batch executor's assembly stage attributes suppression skips with
    /// this so the stamping read stays single-sited (the fan-out's step-1
    /// read): a not-due suppressed path is skipped by both projections (no
    /// record is lost), while a retry-due path is admitted by the assembly
    /// AND by the fan-out's stamping read — the Python retry semantics, with
    /// no double-read re-suppression.
    #[must_use]
    pub fn is_suppressed_without_stamp(
        &self,
        candidate_id: CandidateId,
        current_block: u64,
    ) -> bool {
        if !self.suppressed.contains(&candidate_id) {
            return false;
        }
        let last_retry = self
            .last_retry_block
            .get(&candidate_id)
            .copied()
            .unwrap_or(0);
        current_block.saturating_sub(last_retry) < PATH_SUPPRESS_RETRY_INTERVAL
    }
}

// ============================================================================
// Dispatcher
// ============================================================================

/// Coordination state for the dispatch loop.
///
/// Bundles the seven mutable containers `consume_result_batches`,
/// `dispatch_profitable_results`, and `monitor_pending_transaction` thread as
/// one unit. The
/// containers are mutated in place by the methods. `PathSuppression` is
/// composed (and delegated) so all dispatch coordination lives behind one
/// object.
#[derive(Debug)]
pub struct Dispatcher {
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
    /// Head-event broadcast: `advance_block` publishes the new height so
    /// monitor tasks wait for the next real head instead of a fixed-interval
    /// sleep. `watch` (not `Notify`) so a height change between a monitor's
    /// check and its `changed().await` is never lost and a late subscriber can
    /// read the current height.
    block_events: watch::Sender<u64>,
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
            pending_pools: HashSet::new(),
            active_tasks: JoinSet::new(),
            current_block: Arc::new(Mutex::new(0)),
            block_events: watch::channel(0).0,
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
            block_events: watch::channel(current_block).0,
            ..Self::default()
        }
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

    /// Release the pools held by a completed/expired tx.
    ///
    /// Nonce lifecycle (land, release, tombstone) is the `NonceAuthority`'s;
    /// the dispatcher releases only the pool mutual-exclusion set here.
    pub fn release_tx(&mut self, tx: &CommittedTx) {
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
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        let guard = self
            .current_block
            .lock()
            .expect("current_block mutex poisoned");
        *guard
    }

    /// Update the current block (read by monitor tasks by reference).
    ///
    /// # Panics
    /// Panics if the internal `Mutex` is poisoned.
    pub fn advance_block(&self, block: u64) {
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        let mut guard = self
            .current_block
            .lock()
            .expect("current_block mutex poisoned");
        *guard = block;
        drop(guard);
        // Publish the head event. Monitor tasks await this instead of a
        // fixed-interval timer; no-op when nothing is subscribed.
        self.block_events.send_replace(block);
    }

    /// Subscribe to block-height events: one notification per
    /// [`Self::advance_block`]. Monitor tasks await the next event and
    /// re-evaluate receipt/expiry at head granularity instead of a fixed
    /// interval. The receiver's value tracks the latest published height.
    #[must_use]
    pub fn block_events(&self) -> watch::Receiver<u64> {
        self.block_events.subscribe()
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

    /// Look up the recorded timestamp for `block` (the in-process `evm` sim
    /// uses it as the EVM `block.timestamp` so the V2 pair's `_update()` does
    /// not overflow `timeElapsed` ). `None` if `block` is outside
    /// the recorded window (falls back to 0 at the sim seam).
    #[must_use]
    pub fn block_timestamp_for(&self, block: u64) -> Option<u64> {
        self.block_times
            .iter()
            .find_map(|(b, ts)| (*b == block).then_some(*ts))
    }

    /// Record per-block percentile fees, pruning to `FEE_HISTORY_WINDOW`.
    /// On overflow the min key (oldest block) is popped.
    pub fn record_priority_fees(&mut self, block: u64, fees: BTreeMap<u64, u128>) {
        self.block_priority_fees.insert(block, fees);
        if self.block_priority_fees.len() > FEE_HISTORY_WINDOW {
            // BTreeMap keeps keys sorted; pop_first removes the oldest block.
            self.block_priority_fees.pop_first();
        }
    }

    /// Return the most-recently recorded per-block percentile fees (the max
    /// block key).
    ///
    /// # Panics
    /// Panics if no fees have been recorded yet — an empty window has no
    /// percentiles to draw bounds from, so the caller must record before
    /// reading; this is a programming error, not a recoverable condition.
    #[must_use]
    pub fn latest_priority_fees(&self) -> &BTreeMap<u64, u128> {
        #[expect(clippy::expect_used)] // ring always holds >= 1 entry (documented)
        let last = self
            .block_priority_fees
            .last_key_value()
            .map(|(_, v)| v)
            .expect("latest_priority_fees called before any record_priority_fees");
        last
    }

    /// Borrow the full per-block priority-fee ring (the
    /// `block -> {percentile: fee}` map). Used by the `PyO3` seam so a driver
    /// that reads the newest block's percentiles can do so on its own — returns
    /// an empty map when nothing has been recorded yet (an empty window is a
    /// state, not an error).
    #[must_use]
    pub fn block_priority_fees(&self) -> &BTreeMap<u64, BTreeMap<u64, u128>> {
        &self.block_priority_fees
    }

    // ── PathSuppression ───────────────────────────────────────────────────
    // `PathSuppression` is not composed into `Dispatcher`: the simulation
    // fan-out takes the standalone `Arc<Mutex<PathSuppression>>` directly and
    // `Dispatcher` never touches it, so the `Dispatcher` lock stays
    // uncontended during the simulation fan-out (the submission monitor tasks
    // that contend for the `Dispatcher` lock never block on suppression
    // bookkeeping). The ledger's per-candidate mutators are reachable only from
    // that pipeline; the `PyO3` seam exposes the cumulative tally.

    // ── inspection (test + monitor parity) ──────────────────────────────

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

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
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
            let path_id = CandidateId::new(path_id, degenbot_execution::DeploymentId::new(1));
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

    #[tokio::test]
    async fn advance_block_broadcasts_head_event() {
        let d = Dispatcher::for_block(100);
        let mut rx = d.block_events();
        // A late subscriber reads the seeded height without an event.
        assert_eq!(*rx.borrow(), 100);
        d.advance_block(101);
        // The published height resolves `changed()` immediately.
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), 101);
        assert_eq!(d.current_block(), 101);
    }

    #[test]
    fn release_tx_releases_pools() {
        let mut d = Dispatcher::default();
        d.reserve_pools(vec![PoolKey::new("poolA"), PoolKey::new("poolB")]);
        assert!(d.is_pool_pending(&PoolKey::new("poolA")));

        let tx = CommittedTx::new(vec![PoolKey::new("poolA"), PoolKey::new("poolB")]);
        d.release_tx(&tx);
        assert!(!d.is_pool_pending(&PoolKey::new("poolA")));
        assert_eq!(d.pending_pool_count(), 0);
    }

    #[test]
    fn for_block_seeds_current_block() {
        let d = Dispatcher::for_block(12345);
        assert_eq!(d.current_block(), 12345);
    }

    /// Suppression is per CANDIDATE: the same solver path simulated at two
    /// deployments carries two failure ledgers, so a failure at one deployment
    /// never benches the other (ADR-098 D4).
    #[test]
    fn suppression_entries_never_collapse_same_path_candidates() {
        let built_in = CandidateId::new(7, degenbot_execution::DeploymentId::new(1));
        let foreign = CandidateId::new(7, degenbot_execution::DeploymentId::new(2));
        let mut s = PathSuppression::new();

        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            s.record_failure(built_in);
        }

        assert!(
            s.is_suppressed(built_in, 0),
            "the failing deployment is benched"
        );
        assert!(
            !s.is_suppressed(foreign, 0),
            "the same path at another deployment was never simulated here"
        );
        assert!(s.is_in_suppressed_set(built_in));
        assert!(!s.is_in_suppressed_set(foreign));

        // Success at the other deployment clears nothing the failing one still
        // owes, and vice versa: the two ledgers move independently.
        s.record_success(foreign);
        assert!(s.is_suppressed(built_in, PATH_SUPPRESS_RETRY_INTERVAL - 1));

        s.record_success(built_in);
        assert!(!s.is_suppressed(built_in, PATH_SUPPRESS_RETRY_INTERVAL - 1));
    }

    #[test]
    fn discard_path_clears_all_tracking() {
        let built_in = CandidateId::new(5, degenbot_execution::DeploymentId::new(1));
        let foreign = CandidateId::new(5, degenbot_execution::DeploymentId::new(2));
        let mut s = PathSuppression::new();
        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            s.record_failure(built_in);
            s.record_failure(foreign);
        }
        assert!(s.is_suppressed(built_in, 0));
        assert!(s.is_suppressed(foreign, 0));

        // Discarding one candidate leaves its same-path sibling alone.
        s.discard(built_in);
        assert!(!s.is_suppressed(built_in, 0));
        assert!(s.is_suppressed(foreign, 0));

        // The engine removes a PATH: every deployment's candidate goes.
        s.discard_path(5);
        assert!(!s.is_suppressed(foreign, 0));

        assert_eq!(s.total_suppressed(), 2); // cumulative, not decremented
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

    // ── test-only seam ────────────────────────────────────────────────────
    impl Dispatcher {
        fn block_priority_fees_len(&self) -> usize {
            self.block_priority_fees.len()
        }
    }
}
