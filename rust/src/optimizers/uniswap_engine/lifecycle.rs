//! Path registration, buffer management, and engine accessors.

use super::{UniswapEngine, MixedPoolRef, MixedPath, ResolvedMixedPath, V2BlockEngine, V3BlockEngine, V4BlockEngine, HashMap, SolvePathResult, BlockMetadata, Address};

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
        self.path_pools.insert(path_id, MixedPath { pools: pool_refs });

        // Resolve the path immediately (no solve yet)
        let mut resolved = ResolvedMixedPath::default();
        if let Some(path) = self.path_pools.get(&path_id) {
            self.resolve_path(&path.pools, &mut resolved);
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
                        self.has_unsent_results = true;
                    }
                }
            }
        }

        path_id
    }

    /// Set the maximum age for buffered events in V3/V4 sub-engines.
    pub const fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_engine.set_event_buffer_max_age(max_age);
        self.v4_engine.set_event_buffer_max_age(max_age);
    }

    /// Flush all buffered events in V3/V4 sub-engines.
    pub fn flush_event_buffer(&mut self) {
        self.v3_engine.flush_event_buffer();
        self.v4_engine.flush_event_buffer();
    }

    /// Get a mutable reference to the V2 engine.
    pub const fn v2_engine(&mut self) -> &mut V2BlockEngine {
        &mut self.v2_engine
    }

    /// Get a mutable reference to the V3 engine.
    pub const fn v3_engine(&mut self) -> &mut V3BlockEngine {
        &mut self.v3_engine
    }

    /// Get a shared reference to the V3 engine.
    #[must_use] 
    pub const fn v3_engine_ref(&self) -> &V3BlockEngine {
        &self.v3_engine
    }

    /// Get a mutable reference to the V4 engine.
    pub const fn v4_engine(&mut self) -> &mut V4BlockEngine {
        &mut self.v4_engine
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

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate `results` for the first time (replaces the
    /// removed `initial_solve`). Subsequent `process_logs` calls use
    /// dependency tracking to only re-solve affected paths.
    pub fn solve_all_paths(&mut self, block_number: u64) {
        // Resolve all paths (no clone — path_pools is immutable)
        for (&path_id, path) in &self.path_pools {
            let mut resolved = ResolvedMixedPath::default();
            self.resolve_path(&path.pools, &mut resolved);
            self.path_resolved.insert(path_id, resolved);
        }

        // Solve all paths
        self.results = self.solve_all();
        self.results_block = block_number;

        // Compute incremental diff and send batch
        // (block metadata is not available at initial solve time)
        self.compute_diff_and_send(&BlockMetadata::default());
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.v2_engine.pool_count()
    }

    /// Number of registered V3 pools.
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.v3_engine.pool_count()
    }

    /// Number of registered V4 pools.
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.v4_engine.pool_count()
    }

    /// Number of registered mixed paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.path_pools.len()
    }

    /// Return the list of registered V4 `PoolManager` addresses.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.v4_engine.registered_pool_managers()
    }
}
