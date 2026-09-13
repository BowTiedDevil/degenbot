//! Path registration, buffer management, and engine accessors.

use super::{Address, ArbitrageEngine, HashMap};
use ::degenbot_solvers::mixed::{PoolHop, SolvePathResult};

/// PRG-4 / IRUMXD: `PathRegistrationError` moved to
/// [`super::path_registry`] (ADR-045 `C4UAFP`); re-exported here at its old
/// path so the `PyO3` mapper (`degenbot-python`) and white-box tests compile
/// unchanged.
pub use super::path_registry::PathRegistrationError;

impl ArbitrageEngine {
    // `derive_hop_type` moved to `SolveCycle` (ADR-045 T4).

    /// Register a mixed path and return its ID.
    ///
    /// Each hop's family is derived from the associated `BotState`'s `PoolEntry`
    /// variant; a `pool_id` not registered in the `BotState` is rejected with a
    /// clear error (ADR-006 D3). The path is resolved immediately. If all
    /// pool states are available, the path is marked valid and will be
    /// solved on the next `rebuild_and_solve_affected` or `solve_all_paths`
    /// call.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any `pool_id` is not registered in the associated
    /// `BotState`.
    pub fn register_path(&mut self, hops: Vec<PoolHop>) -> Result<u64, PathRegistrationError> {
        self.cycle
            .register_path(hops, &mut self.registry)
            .map(|r| r.path_id)
    }

    /// Register a path and eagerly solve it.
    ///
    /// Like `register_path`, but also solves the path immediately and
    /// appends the result to `self.cycle.results`. The `pending_new_paths`
    /// set tracks the path so the next `rebuild_and_solve_affected`
    /// merge doesn't discard it.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any `pool_id` is not registered in the associated
    /// `BotState` (see [`register_path`](Self::register_path)).
    pub fn register_and_solve_path(
        &mut self,
        hops: Vec<PoolHop>,
    ) -> Result<u64, PathRegistrationError> {
        self.cycle
            .register_and_solve_path(hops, &mut self.registry)
            .map(|r| r.path_id)
    }

    /// Set the maximum age for buffered events in the V3/V4 buffers
    /// (ADR-003: both live on `BotState`).
    ///
    /// LPEOBI: caches the stance ON the engine - with `None` the expiry is
    /// a provable no-op and `solve_dirty` skips the core write entirely (each
    /// write bought a ~2.9s writer-queue slot under the block-apply stream).
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.event_buffer_expiry_enabled = max_age.is_some();
        self.core.write().set_v3_buffer_max_age(max_age);
        self.core.write().set_v4_buffer_max_age(max_age);
    }

    /// Flush all buffered events in the V3/V4 buffers on `BotState` (ADR-003).
    pub fn flush_event_buffer(&mut self) {
        self.core.write().flush_v3_buffer();
        self.core.write().flush_v4_buffer();
    }

    /// Read the last solved results and block number.
    ///
    /// RAYPAR engine-shard T1 (C42WKO): snaps a snapshot of the `DashMap`
    /// shards into an owned `HashMap` so the caller never holds a lock
    /// into the engine. `O(n_results)` — typically <50 entries (profitable
    /// solves only) per drain.
    #[must_use]
    pub fn latest_results(&self) -> (HashMap<u64, SolvePathResult>, u64) {
        (
            self.cycle
                .results
                .iter()
                .map(|r| (*r.key(), r.value().clone()))
                .collect(),
            self.cycle.cursor.results_block(),
        )
    }

    /// Return the last block number processed by `process_block`.
    /// Returns `None` if no block has been processed yet.
    #[must_use]
    pub const fn last_processed_block(&self) -> Option<u64> {
        self.cycle.cursor.last_processed_block()
    }

    /// Set the last processed block manually.
    ///
    /// Called by Python after backfill completes, so the Rust pump
    /// knows not to re-process the backfilled range. Without this,
    /// the pump would restart from `first_observed_block` and buffer
    /// events that the Python pools already reflect, causing
    /// double-application when pools are later registered.
    ///
    /// 6XB6NJ: a monotone advance on the block cursor — a lower value
    /// cannot pull the processed boundary backwards.
    pub fn set_last_processed_block(&mut self, block: u64) {
        self.cycle.cursor.advance_processed(block);
    }

    /// The last block this engine's `finalize_block` guard advanced past.
    /// Owned by the engine since ergo task LEZJAS (the pump's `&mut` out-
    /// params retired); enables a mid-flight engine to pick up the pump's last
    /// solved block on join (ADR-006 D4). Starts at 0 (so the first header /
    /// tombstone `finalize_block(block > 0)` fires).
    #[must_use]
    pub const fn last_solved_block(&self) -> u64 {
        self.cycle.cursor.last_solved_block()
    }

    /// Seed the engine's `last_solved_block` (e.g. on mid-flight join: a late
    /// engine inherits the pump's current solved block). Test helper too — the
    /// `finalize_block_threads_metadata_into_send` test pre-seeds 0 to fire the
    /// guard. Production pump path lets `finalize_block` advance it.
    ///
    /// 6XB6NJ: a monotone advance on the block cursor. Behavior-preserving
    /// on every existing call path (the ADR-006 D4 inherit + tests): the
    /// engine starts at 0 and the production stamps are non-decreasing, so
    /// the max is the same value the old unconditional write landed.
    pub fn set_last_solved_block(&mut self, block: u64) {
        self.cycle.cursor.advance_solved_boundary(block);
    }

    /// Seed the cold-start `results_block` anchor to a **settled** block (the
    /// pump calls this at resume with the backfill/resume boundary). Backfill
    /// deliberately does not solve and `register_and_solve_path` eager-solves
    /// without advancing `results_block`, so before the first real `on_drain`
    /// it is `0`. Without a seed, delivery would either publish at block 0 (the
    /// strategy sims every tracked pool as an EOA → code-less panic) or defer
    /// every registration eager-solve until the first dirty event (losing a
    /// capturable window). Seeding `results_block` to the settled resume block
    /// — a completed, fully-applied block within the backfill window — lets
    /// cold-start candidates deliver immediately at a valid, verification-safe
    /// solve block.
    ///
    /// 6XB6NJ: a plain monotone advance on the block cursor — the old
    /// only-if-zero guard is subsumed ("never regress" holds by
    /// construction; see `BlockCursor::advance_solved`).
    pub fn set_solve_anchor(&mut self, block: u64) {
        self.cycle.cursor.advance_solved(block);
    }

    /// KJWIK5: install the deferred-path re-record hook (the ledger carry).
    /// The `EngineStages` constructor is the production installer — it
    /// captures
    /// the shared `EpochDelta` and re-records a deferred path's hop-pool
    /// keys at the cycle's solve block. Direct engine drives (unit tests, the
    /// cold-start `solve_all`) leave it unset, and the deferral falls back
    /// to today's dropped behavior.
    pub(crate) fn set_deferred_re_record(&mut self, hook: super::DeferredReRecordHook) {
        self.cycle.deferred_re_record = Some(hook);
    }

    /// Whether any forward log applied since the last `finalize_block` (the
    /// pump's forward-log path calls this before the next `finalize_block` so
    /// the empty-block branch sends the advance diff). Owned by the engine
    /// since LEZJAS; returns `false` until the first `record_logs_this_block`.
    #[must_use]
    pub const fn has_logs_this_block(&self) -> bool {
        self.cycle.cursor.has_logs_this_block()
    }

    /// Record that at least one forward log applied this block (clears on the
    /// next `finalize_block`). Replaces the pump's `has_logs_this_block = true;`
    /// out-param write (ergo task LEZJAS).
    pub fn record_logs_this_block(&mut self) {
        self.cycle.cursor.record_logs();
    }

    /// Resolve and solve all registered paths. **Solve-only — does NOT dispatch
    /// a batch** (matches `solve_dirty`'s contract; dispatch is the pump's job
    /// via `send_result_batch`, driven by the debounce timer).
    ///
    /// Cold-start / test synchronization entry point (replaces the removed
    /// `initial_solve`). Populates `self.cycle.results` and advances `results_block`;
    /// leaves `delivered` untouched (Python has not yet received anything —
    /// `delivered`'s invariant is "what Python has seen via the channel," and
    /// that stays empty until the pump's first real send). Subsequent
    /// `process_logs` calls use dependency tracking to only re-solve affected
    /// paths.
    ///
    /// Callers read results via `latest_results()`; none reads a dispatched
    /// `ResultBatch` from this entry (grep-verified across `tests/`, `examples/`,
    /// and `src/degenbot/`).
    #[tracing::instrument(name = "degenbot.arb.solve_all", skip(self), fields(block_number, path_count = self.registry.len()))]
    pub fn solve_all_paths(&mut self, block_number: u64) {
        self.cycle.solve_all_paths(block_number, &self.registry);
    }

    /// Number of registered V2 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.core.read().v2_pool_count()
    }

    /// Number of registered V3 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.core.read().v3_pool_count()
    }

    /// Number of registered V4 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.core.read().v4_pool_count()
    }

    /// Number of registered mixed paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.registry.len()
    }

    /// PRG-4 / IRUMXD: the engine path registry owns the registered-path cap
    /// (was the Python `MAX_REGISTERED_PATHS` counter). `None` = unlimited.
    /// The `PyO3` driver sets it once at boot from the typed config value.
    pub fn set_path_cap(&mut self, cap: Option<usize>) {
        self.registry.set_cap(cap);
    }

    /// PRG-4: dedup hits counted engine-side — a duplicate registration
    /// returns the existing id and never surfaces to the driver as a skip,
    /// so the `dup` telemetry needs this witness.
    #[must_use]
    pub fn path_dedups(&self) -> u64 {
        self.registry.dedups()
    }

    /// Total actual hop projections performed (cache misses) since engine
    /// construction. Test + telemetry observable for the projection memo.
    #[must_use]
    pub fn hop_projection_count(&self) -> u64 {
        self.cycle.hop_projection_count
    }

    /// Return the list of registered V4 `PoolManager` addresses.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.core.read().v4_registered_pool_managers()
    }
}
