//! Path registration, buffer management, and engine accessors.

use super::{Address, ArbitrageEngine, HashMap};
use crate::bot_core::resolve::resolve_hops;
use crate::bot_core::BotState;
use ::degenbot_solvers::mixed::{
    HopType, MixedPath, MixedPoolRef, PoolHop, ResolvedMixedPath, SolvePathResult,
};

impl ArbitrageEngine {
    /// Derive a hop's family from the `BotState`'s `PoolEntry` variant.
    ///
    /// Returns `None` if `pool_id` isn't registered in `core` — the caller
    /// (`register_path`) rejects such hops with a clear error (ADR-006 D3:
    /// the engine never constructs pools, so it learns each hop's family
    /// from the `BotState` that owns it).
    fn derive_hop_type(core: &BotState, pool_id: u64) -> Option<HopType> {
        // Aerodrome stable pools route to the Solidly solve branch; volatile
        // Aerodrome is constant-product and routes to the V2 (Möbius) branch
        // (matching the Python `arbitrage.solvers.solidly_stable` classification:
        // `AerodromeV2Pool(stable=True)` → `SolidlyStableHop`, else
        // `ConstantProductHop`).
        if let Some(id) = core.get_aerodrome_identity(pool_id) {
            return Some(if id.stable {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        // Camelot stable_swap pools route to the Solidly solve branch;
        // volatile Camelot is constant-product (V2). Same Python-faithful
        // classification as Aerodrome.
        if let Some(id) = core.get_v2_identity(pool_id) {
            return Some(if id.stable_swap {
                HopType::SolidlyStable
            } else {
                HopType::V2
            });
        }
        if core.get_v3_pool(pool_id).is_some() {
            Some(HopType::V3)
        } else if core.get_v4_pool(pool_id).is_some() {
            Some(HopType::V4)
        } else if core.get_balancer_weighted_pool(pool_id).is_some() {
            Some(HopType::BalancerWeighted)
        } else if core.get_balancer_stable_pool(pool_id).is_some() {
            Some(HopType::BalancerStable)
        } else if core.get_curve_pool(pool_id).is_some() {
            Some(HopType::CurveStableswap)
        } else {
            None
        }
    }

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
    pub fn register_path(&mut self, hops: Vec<PoolHop>) -> Result<u64, String> {
        // R522XA: fewer than two hops is a structural caller bug, not a state —
        // reject loudly at construction.
        if hops.len() < 2 {
            return Err(format!(
                "register_path: path has {} hops (need >= 2) — structurally unroutable",
                hops.len()
            ));
        }

        // Telemetry: one Jaeger node per path registration (a root span on the
        // registration worker thread — there is no ambient pump context during
        // `build_paths`). The completion event below carries the CONCRETE hop
        // list so the trace answers "which pools are in this path" directly.
        // FPGOYX: dedup — if the same (pool_id, zero_for_one) sequence is
        // already registered, return the existing path_id instead of creating
        // a duplicate. Without this, `build_paths` re-entry accumulated
        // hundreds of thousands of duplicate paths, OOM-killing the bot.
        let sig: Vec<(u64, bool)> = hops.iter().map(|h| (h.pool_id, h.zero_for_one)).collect();
        if let Some(&existing_id) = self.path_signatures.get(&sig) {
            tracing::debug!(
                target: "degenbot::path",
                path_id = existing_id,
                hops.count = hops.len(),
                "[path] duplicate registration skipped (dedup)"
            );
            return Ok(existing_id);
        }

        let reg_span = tracing::info_span!("degenbot.path.register", hops.count = hops.len());
        let _reg_guard = reg_span.enter();
        // Resolve each hop's family from the BotState + validate the pool_id
        // exists there. The engine never constructs pools (ADR-006 D3), so
        // hop_type is derived, not caller-supplied.
        let mut pool_refs = Vec::with_capacity(hops.len());
        let mut hop_descs = Vec::with_capacity(hops.len());
        {
            let core = self.core.read();
            for hop in hops {
                let Some(hop_type) = Self::derive_hop_type(&core, hop.pool_id) else {
                    return Err(format!(
                        "register_path: pool_id {} is not registered in the associated BotState",
                        hop.pool_id
                    ));
                };
                hop_descs.push(super::path_info::describe_hop(
                    &core,
                    hop_type,
                    hop.pool_id,
                    hop.zero_for_one,
                ));
                pool_refs.push(MixedPoolRef {
                    hop_type,
                    pool_key: hop.pool_id,
                    zero_for_one: hop.zero_for_one,
                });
            }
        }

        // R522XA: resolve BEFORE storing so an unroutable hop rejects the
        // registration loudly and leaves no half-registered state behind.
        let mut resolved = ResolvedMixedPath::default();
        let deficits = {
            let core = self.core.read();
            resolve_hops(
                &core,
                &pool_refs,
                &mut resolved,
                &mut self.hop_projection_cache,
                Some(&mut self.hop_projection_count),
                self.cl_projection_memo,
            )
        };
        if let Some(unroutable) = deficits
            .iter()
            .find(|d| d.reason.is_structurally_unroutable())
        {
            return Err(format!(
                "register_path: hop ({hop_type:?} pool {pool_key}) is structurally unroutable ({reason}) — rejecting path at construction",
                hop_type = format!("{:?}", unroutable.hop_type),
                pool_key = unroutable.pool_key,
                reason = unroutable.reason,
            ));
        }

        // Only now allocate the path id (no gaps from rejected registrations)
        // and store the immutable pool refs + reverse index.
        let path_id = self.next_path_id;
        self.next_path_id += 1;
        for pool_ref in &pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }
        self.path_pools
            .insert(path_id, MixedPath { pools: pool_refs });

        // Store the resolve snapshot + drive the state machine.
        let path_valid = resolved.valid;
        self.path_resolved.insert(path_id, resolved);
        self.path_status
            .entry(path_id)
            .or_default()
            .set_resolved(&deficits);
        self.path_signatures.insert(sig, path_id);

        tracing::info!(
            target: "degenbot::path",
            path_id = path_id,
            hops.count = hop_descs.len(),
            hops = %hop_descs.join(" -> "),
            valid = path_valid,
            "[path] registered"
        );

        Ok(path_id)
    }

    /// Register a path and eagerly solve it.
    ///
    /// Like `register_path`, but also solves the path immediately and
    /// appends the result to `self.results`. The `pending_new_paths`
    /// set tracks the path so the next `rebuild_and_solve_affected`
    /// merge doesn't discard it.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any `pool_id` is not registered in the associated
    /// `BotState` (see [`register_path`](Self::register_path)).
    pub fn register_and_solve_path(&mut self, hops: Vec<PoolHop>) -> Result<u64, String> {
        let path_id = self.register_path(hops)?;

        // Eagerly solve the newly registered path
        if let Some(resolved) = self.path_resolved.get(&path_id) {
            if resolved.valid {
                if let Some(mut solve_result) = ::degenbot_solvers::mixed::solve_path(
                    resolved,
                    &::degenbot_solvers::profit_envelope::GateDeps::offline(),
                )
                .result
                {
                    if !solve_result.optimal_input.is_zero() && !solve_result.profit.is_zero() {
                        self.clamp_cl_hop_capacity(path_id, &mut solve_result);
                        self.results.insert(path_id, solve_result);
                        self.pending_new_paths.insert(path_id);
                    }
                }
            }
        }

        Ok(path_id)
    }

    /// Set the maximum age for buffered events in the V3/V4 buffers
    /// (ADR-003: both live on `BotState`).
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
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
            self.results
                .iter()
                .map(|r| (*r.key(), r.value().clone()))
                .collect(),
            self.results_block,
        )
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

    /// The last block this engine's `finalize_block` guard advanced past.
    /// Owned by the engine since ergo task LEZJAS (the pump's `&mut` out-
    /// params retired); enables a mid-flight engine to pick up the pump's last
    /// solved block on join (ADR-006 D4). Starts at 0 (so the first header /
    /// tombstone `finalize_block(block > 0)` fires).
    #[must_use]
    pub const fn last_solved_block(&self) -> u64 {
        self.last_solved_block
    }

    /// Seed the engine's `last_solved_block` (e.g. on mid-flight join: a late
    /// engine inherits the pump's current solved block). Test helper too — the
    /// `finalize_block_threads_metadata_into_send` test pre-seeds 0 to fire the
    /// guard. Production pump path lets `finalize_block` advance it.
    pub fn set_last_solved_block(&mut self, block: u64) {
        self.last_solved_block = block;
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
    /// Only fills when `results_block` is still `0`: once a real solve has
    /// established a (possibly higher) anchor, we never regress it.
    pub fn set_solve_anchor(&mut self, block: u64) {
        if self.results_block == 0 {
            self.results_block = block;
        }
    }

    /// Whether any forward log applied since the last `finalize_block` (the
    /// pump's forward-log path calls this before the next `finalize_block` so
    /// the empty-block branch sends the advance diff). Owned by the engine
    /// since LEZJAS; returns `false` until the first `record_logs_this_block`.
    #[must_use]
    pub const fn has_logs_this_block(&self) -> bool {
        self.has_logs_this_block
    }

    /// Record that at least one forward log applied this block (clears on the
    /// next `finalize_block`). Replaces the pump's `has_logs_this_block = true;`
    /// out-param write (ergo task LEZJAS).
    pub fn record_logs_this_block(&mut self) {
        self.has_logs_this_block = true;
    }

    /// Resolve and solve all registered paths. **Solve-only — does NOT dispatch
    /// a batch** (matches `solve_dirty`'s contract; dispatch is the pump's job
    /// via `send_result_batch`, driven by the debounce timer).
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
    #[tracing::instrument(skip(self), fields(block_number, path_count = self.path_pools.len()))]
    pub fn solve_all_paths(&mut self, block_number: u64) {
        // Resolve all paths under the core lock (single consistent snapshot of
        // all family state — ADR-003).
        {
            let core = self.core.read();
            for (&path_id, path) in &self.path_pools {
                let mut resolved = ResolvedMixedPath::default();
                let deficits = resolve_hops(
                    &core,
                    &path.pools,
                    &mut resolved,
                    &mut self.hop_projection_cache,
                    Some(&mut self.hop_projection_count),
                    self.cl_projection_memo,
                );
                self.path_resolved.insert(path_id, resolved);
                // R522XA: cold-start full sweep also refreshes the state machine.
                self.path_status
                    .entry(path_id)
                    .or_default()
                    .set_resolved(&deficits);
            }
        }

        // Solve all paths
        let results = self.solve_all();
        self.results.clear();
        for (pid, r) in results {
            self.results.insert(pid, r);
        }
        self.results_block = block_number;

        // Intentionally no compute_diff_and_send here: dispatching would
        // advance `delivered` (claiming "Python has seen these") before any
        // channel exists — poisoning the diff for the first real send. The
        // pump owns dispatch via `send_result_batch`.
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
        self.path_pools.len()
    }

    /// Total actual hop projections performed (cache misses) since engine
    /// construction. Test + telemetry observable for the projection memo.
    #[must_use]
    pub fn hop_projection_count(&self) -> u64 {
        self.hop_projection_count
    }

    /// Return the list of registered V4 `PoolManager` addresses.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.core.read().v4_registered_pool_managers()
    }
}
