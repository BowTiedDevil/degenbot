//! Path registration, buffer management, and engine accessors.

use super::{
    Address, HashMap, MixedPath, MixedPoolRef, ResolvedMixedPath, SolvePathResult, UniswapEngine,
};

impl UniswapEngine {
    /// Register a mixed path and return its ID.
    ///
    /// The path is resolved immediately. If all pool states are available,
    /// the path is marked valid and will be solved on the next
    /// `rebuild_and_solve_affected` or `solve_all_paths` call.
    pub fn register_path(&mut self, pool_refs: Vec<MixedPoolRef>) -> u64 {
        let path_id = self.next_path_id;
        self.next_path_id += 1;

        // Build the reverse index entries
        for pool_ref in &pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }

        // Store the immutable pool refs
        self.path_pools
            .insert(path_id, MixedPath { pools: pool_refs });

        // Resolve the path immediately (no solve yet). V2 state is read from
        // Bot under the core lock (ADR-003).
        let mut resolved = ResolvedMixedPath::default();
        if let Some(path) = self.path_pools.get(&path_id) {
            let core = self.core.lock();
            self.resolve_path(&core, &path.pools, &mut resolved);
        }
        self.path_resolved.insert(path_id, resolved);

        path_id
    }

    /// Register a path and eagerly solve it.
    ///
    /// Like `register_path`, but also solves the path immediately and
    /// appends the result to `self.results`. The `pending_new_paths`
    /// set tracks the path so the next `rebuild_and_solve_affected`
    /// merge doesn't discard it.
    pub fn register_and_solve_path(&mut self, pool_refs: Vec<MixedPoolRef>) -> u64 {
        let path_id = self.register_path(pool_refs);

        // Eagerly solve the newly registered path
        if let Some(resolved) = self.path_resolved.get(&path_id) {
            if resolved.valid {
                if let Some(solve_result) = self.solve_path(resolved) {
                    if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                        self.results.insert(path_id, solve_result);
                        self.pending_new_paths.insert(path_id);
                    }
                }
            }
        }

        path_id
    }

    /// Set the maximum age for buffered events in the V3/V4 buffers
    /// (ADR-003: both live on `Bot`).
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.core.lock().set_v3_buffer_max_age(max_age);
        self.core.lock().set_v4_buffer_max_age(max_age);
    }

    /// Flush all buffered events in the V3/V4 buffers on `Bot` (ADR-003).
    pub fn flush_event_buffer(&mut self) {
        self.core.lock().flush_v3_buffer();
        self.core.lock().flush_v4_buffer();
    }

    /// Read the last solved results and block number.
    #[must_use]
    pub const fn latest_results(&self) -> (&HashMap<u64, SolvePathResult>, u64) {
        (&self.results, self.results_block)
    }

    /// Return the last block number processed by `process_block`.
    /// Returns `None` if no block has been processed yet.
    #[must_use]
    pub const fn last_processed_block(&self) -> Option<u64> {
        self.last_processed_block
    }

    /// Set the last processed block manually.
    ///
    /// Called by Python after backfill completes, so the Rust pump
    /// knows not to re-process the backfilled range. Without this,
    /// the pump would restart from `first_observed_block` and buffer
    /// events that the Python pools already reflect, causing
    /// double-application when pools are later registered.
    pub const fn set_last_processed_block(&mut self, block: u64) {
        self.last_processed_block = Some(block);
    }

    /// Resolve and solve all registered paths. **Solve-only — does NOT dispatch
    /// a batch** (matches `solve_dirty`'s contract; dispatch is the pump's job
    /// via `send_result_batch`, driven by the 50ms debounce timer).
    ///
    /// Cold-start / test synchronization entry point (replaces the removed
    /// `initial_solve`). Populates `self.results` and advances `results_block`;
    /// leaves `delivered` untouched (Python has not yet received anything —
    /// `delivered`'s invariant is "what Python has seen via the channel," and
    /// that stays empty until the pump's first real send). Subsequent
    /// `process_logs` calls use dependency tracking to only re-solve affected
    /// paths.
    ///
    /// Callers read results via `latest_results()`; none reads a dispatched
    /// `ResultBatch` from this entry (grep-verified across `tests/`, `examples/`,
    /// and `src/degenbot/`).
    pub fn solve_all_paths(&mut self, block_number: u64) {
        // Resolve all paths under the core lock (single consistent V2
        // snapshot). V3/V4 state still reads the per-family block engines.
        {
            let core = self.core.lock();
            for (&path_id, path) in &self.path_pools {
                let mut resolved = ResolvedMixedPath::default();
                self.resolve_path(&core, &path.pools, &mut resolved);
                self.path_resolved.insert(path_id, resolved);
            }
        }

        // Solve all paths
        self.results = self.solve_all();
        self.results_block = block_number;

        // Intentionally no compute_diff_and_send here: dispatching would
        // advance `delivered` (claiming "Python has seen these") before any
        // channel exists — poisoning the diff for the first real send. The
        // pump owns dispatch via `send_result_batch`.
    }

    /// Number of registered V2 pools (state lives in `Bot` under ADR-003).
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.core.lock().v2_pool_count()
    }

    /// Number of registered V3 pools (state lives in `Bot` under ADR-003).
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.core.lock().v3_pool_count()
    }

    /// Number of registered V4 pools (state lives in `Bot` under ADR-003).
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.core.lock().v4_pool_count()
    }

    /// Number of registered mixed paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.path_pools.len()
    }

    /// Return the list of registered V4 `PoolManager` addresses.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.core.lock().v4_registered_pool_managers()
    }
}
