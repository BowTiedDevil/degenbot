//! Path registration, buffer management, and engine accessors.
use super::solve_cycle::{INLINE_SIM_ENABLED, MIN_PROFIT_FLOOR_WEI};
use super::{Address, ArbitrageEngine, HashMap};
use ::degenbot_solvers::mixed::{PoolHop, SolvePathResult};
use alloy::primitives::U256;
/// KAHU5W: the solver crate's runtime stance is INSTANCE-SCOPED — built
/// fresh per engine from the typed config and passed down; no `OnceLock`.
#[must_use]
pub(crate) fn solve_runtime_config_from_cfg(
    cfg: &::degenbot_config::BotConfig,
) -> ::degenbot_solvers::runtime::SolveRuntimeConfig {
    ::degenbot_solvers::runtime::SolveRuntimeConfig {
        event_solver_legacy: cfg.solve.walk_event_solver_legacy,
        walk_event_census: cfg.solve.walk_event_census,
        anchor_sweep: match cfg.solve.walk_anchor_sweep {
            ::degenbot_config::AnchorSweep::Off => ::degenbot_solvers::runtime::AnchorSweep::Off,
            ::degenbot_config::AnchorSweep::CenterOnly => {
                ::degenbot_solvers::runtime::AnchorSweep::CenterOnly
            }
            ::degenbot_config::AnchorSweep::Full => ::degenbot_solvers::runtime::AnchorSweep::Full,
        },
        max_tangent_lines: cfg.solve.envelope_max_tangent_lines,
        sampled_compose_lines: cfg.solve.envelope_sampled_compose_lines,
        memo_on: cfg.solve.solver_walk_memo,
        memo_stats: cfg.solve.solver_walk_memo_stats,
    }
}
/// T4 (KAHU5W): the ONE config parse point for the engine's runtime stances —
/// called at engine construction with the typed `BotConfig`; hot paths read
/// the parsed statics. The crate performs ZERO environment reads: every stance
/// is a schema key (env or TOML loads into it via the degenbot-config loader).
/// The solver-runtime stance is NOT installed globally anymore — the engine
/// holds an instance value built by [`solve_runtime_config_from_cfg`] and
/// threads it down (KAHU5W: the solver `OnceLock` is retired).
///
/// YI5NGB: the boots installed here are the CONSTRUCTION-STAMPED values —
/// the engine derived its own `FleetBoot` from THIS caller cfg and stamped it
/// (`BootStamp`: engine id + deterministic cfg hash); the per-role fleet
/// executors courier the identified stamp to the single fleet
/// materialization, and a divergent-cfg rider is ledgered
/// (`boot_stamp::record_ride`) instead of silently winning the fleet.
pub(crate) fn install_engine_stances(
    cfg: &::degenbot_config::BotConfig,
    boot_stamp: &crate::arb_engine::boot_stamp::BootStamp,
) {
    // LW-T9 (no stance, no migration flag): the solve bins ALWAYS ride the
    // fleet-hosted executor; the typed boot descriptor (quota + overrides +
    // posture) is parsed here once.
    // YI5NGB: the engine's OWN construction boot, stamped — each role's
    // install records the identified ride (first-fleet-wins per role).
    crate::arb_engine::fleet_solve_executor::install_boot(boot_stamp.clone());
    // candidate 4 (YUMQU3): the two POOLED roles install through the ONE
    // registry. Sim installs BEFORE registration, so the registry's
    // first-wins canonical process boot is sim's (same descriptor value as
    // registration's — the boot is shared). ADR-042 F4: the SimDriver seat
    // pool shares the boot descriptor; PRG-3: registration shares it too
    // (duty-counted PoolStateUpdater slots, Deferrable cordon class).
    let registry = crate::arb_engine::seat_host::FleetBootRegistry::process();
    registry.install_boot(
        crate::arb_engine::boot_stamp::BootRole::Sim,
        boot_stamp.clone(),
    );
    registry.install_boot(
        crate::arb_engine::boot_stamp::BootRole::Registration,
        boot_stamp.clone(),
    );
    // J4HN66: streaming/detached stances are per-engine cfg values now
    // (packed at construction); this install keeps only the statics that
    // still have non-construction consumers (INLINE_SIM).
    INLINE_SIM_ENABLED.store(
        cfg.solve.solve_inline_sim,
        std::sync::atomic::Ordering::Relaxed,
    );
    let min_profit = U256::from(cfg.solve.min_profit_wei);
    let _ = MIN_PROFIT_FLOOR_WEI.set(min_profit);
    crate::bot_core::resolve::install_projection_memo_stance(cfg.solve.cl_projection_cache);
    // 7LV6VN T2 (YI5NGB): the chunked parallel resolve stance is an ENGINE
    // instance value now — packed per construction from
    // cfg.solve.solve_resolve_par (the KAHU5W construction-stance
    // trajectory); no installer store remains here.
}
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
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
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
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
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
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
    pub fn set_event_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.event_buffer_expiry_enabled = max_age.is_some();
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .set_v3_buffer_max_age(max_age);
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .set_v4_buffer_max_age(max_age);
    }
    /// Flush all buffered events in the V3/V4 buffers on `BotState` (ADR-003).
    pub fn flush_event_buffer(&mut self) {
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .flush_v3_buffer();
        self.core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .flush_v4_buffer();
    }
    /// Read the last solved results and block number.
    ///
    /// RAYPAR engine-shard T1 (C42WKO): snaps a snapshot of the `DashMap`
    /// shards into an owned `HashMap` so the caller never holds a lock
    /// into the engine. `O(n_results)` — typically <50 entries (profitable
    /// solves only) per drain.
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
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
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
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
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
    #[tracing::instrument(name = "degenbot.arb.solve_all", skip(self), fields(block_number, path_count = self.registry.len()))]
    pub fn solve_all_paths(&mut self, block_number: u64) {
        self.cycle.solve_all_paths(block_number, &self.registry);
    }
    /// Number of registered V2 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.core
            .read_at(crate::bot_core::state_lock::LockSite::Solver)
            .v2_pool_count()
    }
    /// Number of registered V3 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.core
            .read_at(crate::bot_core::state_lock::LockSite::Solver)
            .v3_pool_count()
    }
    /// Number of registered V4 pools (state lives in `BotState` under ADR-003).
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.core
            .read_at(crate::bot_core::state_lock::LockSite::Solver)
            .v4_pool_count()
    }
    /// Number of registered mixed paths.
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.registry.len()
    }
    /// PRG-4 / IRUMXD: the engine path registry owns the registered-path cap
    /// (was the Python `MAX_REGISTERED_PATHS` counter). `None` = unlimited.
    /// The `PyO3` driver sets it once at boot from the typed config value.
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
    pub fn set_path_cap(&mut self, cap: Option<usize>) {
        self.registry.set_cap(cap);
    }
    /// PRG-4: dedup hits counted engine-side — a duplicate registration
    /// returns the existing id and never surfaces to the driver as a skip,
    /// so the `dup` telemetry needs this witness.
    /// T5 rehome target: thin engine casing for the `PyO3` driver until T5 re-sources it onto `EngineStages`.
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
        self.core
            .read_at(crate::bot_core::state_lock::LockSite::Solver)
            .v4_registered_pool_managers()
    }
}
/// The detached solve cycle (enqueue-and-return with sidecar merge) is THE
/// ONLY solve arm since the WFF6MM hard cutover: the DRIVEN solve path takes
/// NO engine-level Mutex — the stage-surface (`EngineStages`) solve hold
/// collapses to enqueue end (µs) and each result merges on the sidecar under
/// its own short per-item acquisition (the Q1a stale policy makes that safe).
/// The `DEGENBOT_DETACHED_SOLVES` stance (and its in-cycle opt-out) retired
/// with the in-cycle arm; backpressure is the admission draw, not the old
/// in-flight cap.
#[cfg(test)]
mod streaming_stance_tests {
    // WFF6MM: the detached-solve stance key retired from the schema; there
    // is no opt-out — the one solve arm is unconditional.
}
