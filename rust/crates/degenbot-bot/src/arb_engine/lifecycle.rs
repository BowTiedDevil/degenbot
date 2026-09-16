//! Path registration, buffer management, and engine accessors.
//!
//! 5TBT7L T6: the inherent `impl ArbitrageEngine` block was dissolved. Every
//! member is now a machine-direct free function over `&ArbitrageEngine` /
//! `&mut ArbitrageEngine`; the stage surface (`EngineStages`) and the in-crate
//! tests both call these directly, so no delegating inherent engine method
//! remains outside `arb_engine/mod.rs` (the one-impl-block structural gate).

use super::{ArbitrageEngine, HashMap};
use ::degenbot_solvers::mixed::{PoolHop, SolvePathResult};
/// the solver crate's runtime stance is INSTANCE-SCOPED — built
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
/// T4: the ONE config parse point for the engine's runtime stances —
/// called at engine construction with the typed `BotConfig`; hot paths read
/// the parsed statics. The crate performs ZERO environment reads: every stance
/// is a schema key (env or TOML loads into it via the degenbot-config loader).
/// The solver-runtime stance is NOT installed globally anymore — the engine
/// holds an instance value built by [`solve_runtime_config_from_cfg`] and
/// threads it down (KAHU5W: the solver `OnceLock` is retired).
///
/// the boots installed here are the CONSTRUCTION-STAMPED values —
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
    // the engine's OWN construction boot, stamped — each role's
    // install records the identified ride (first-fleet-wins per role).
    crate::arb_engine::fleet_solve_executor::install_boot(boot_stamp.clone());
    // candidate 4: the two POOLED roles install through the ONE
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
    // C2: the cycle's stance values are ENGINE instance values now
    // (`SolveCycle::min_profit_floor` / `::inline_sim_enabled`, packed at
    // construction) — no process statics remain for the solve cycle.
    crate::bot_core::resolve::install_projection_memo_stance(cfg.solve.cl_projection_cache);
    // the chunked parallel resolve stance is an ENGINE
    // instance value now — packed per construction from
    // cfg.solve.solve_resolve_par (the KAHU5W construction-stance
    // trajectory); no installer store remains here.
}
/// PRG-4 / IRUMXD: `PathRegistrationError` moved to
/// [`super::path_registry`] (ADR-045 `C4UAFP`); re-exported here at its old
/// path so the `PyO3` mapper (`degenbot-python`) and white-box tests compile
/// unchanged.
pub use super::path_registry::PathRegistrationError;

/// Register a mixed path and return its ID (machine-direct free-function route).
///
/// Each hop's family is derived from the associated `BotState`'s `PoolEntry`
/// variant; a `pool_id` not registered in the `BotState` is rejected with a
/// clear error (ADR-006 D3). The path is resolved immediately. If all pool
/// states are available, the path is marked valid and will be solved on the
/// next dirty cycle or `solve_all_paths` call.
///
/// # Errors
///
/// Returns `Err` if any `pool_id` is not registered in the associated
/// `BotState`.
pub(crate) fn register_path(
    engine: &mut ArbitrageEngine,
    hops: Vec<PoolHop>,
) -> Result<u64, PathRegistrationError> {
    engine
        .cycle
        .register_path(hops, &mut engine.registry)
        .map(|r| r.path_id)
}
/// Register a path and eagerly solve it (machine-direct free-function route).
///
/// Like `register_path`, but also solves the path immediately and appends the
/// result to `engine.cycle.results`. The `pending_new_paths` set tracks the
/// path so the next dirty-cycle merge doesn't discard it.
///
/// # Errors
///
/// Returns `Err` if any `pool_id` is not registered in the associated
/// `BotState` (see [`register_path`]).
pub(crate) fn register_and_solve_path(
    engine: &mut ArbitrageEngine,
    hops: Vec<PoolHop>,
) -> Result<u64, PathRegistrationError> {
    engine
        .cycle
        .register_and_solve_path(hops, &mut engine.registry)
        .map(|r| r.path_id)
}
/// Set the maximum age for buffered events in the V3/V4 buffers (ADR-003:
/// both live on `BotState`).
///
/// LPEOBI: caches the stance ON the engine - with `None` the expiry is a
/// provable no-op and `expire_buffered_events` skips the core write (each
/// write bought a ~2.9s writer-queue slot under the block-apply stream).
pub(crate) fn set_event_buffer_max_age(engine: &mut ArbitrageEngine, max_age: Option<u64>) {
    engine.event_buffer_expiry_enabled = max_age.is_some();
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .set_v3_buffer_max_age(max_age);
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .set_v4_buffer_max_age(max_age);
}
/// Flush all buffered events in the V3/V4 buffers on `BotState` (ADR-003).
pub(crate) fn flush_event_buffer(engine: &mut ArbitrageEngine) {
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .flush_v3_buffer();
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .flush_v4_buffer();
}
/// Read the last solved results and block number.
///
/// RAYPAR engine-shard T1: snaps a snapshot of the `DashMap`
/// shards into an owned `HashMap` so the caller never holds a lock into the
/// engine. `O(n_results)` — typically <50 entries (profitable solves only)
/// per drain.
#[must_use]
pub(crate) fn latest_results(engine: &ArbitrageEngine) -> (HashMap<u64, SolvePathResult>, u64) {
    (
        engine
            .cycle
            .results
            .iter()
            .map(|r| (*r.key(), r.value().clone()))
            .collect(),
        engine.cycle.cursor.results_block(),
    )
}
/// Return the last block number processed by `process_block`.
/// Returns `None` if no block has been processed yet.
#[must_use]
pub(crate) fn last_processed_block(engine: &ArbitrageEngine) -> Option<u64> {
    engine.cycle.cursor.last_processed_block()
}
/// Set the last processed block manually (machine-direct cursor poke).
///
/// Called by Python after backfill completes, so the Rust pump knows not to
/// re-process the backfilled range. 6XB6NJ: a monotone advance on the block
/// cursor — a lower value cannot pull the processed boundary backwards.
pub(crate) fn set_last_processed_block(engine: &mut ArbitrageEngine, block: u64) {
    engine.cycle.cursor.advance_processed(block);
}
/// install the deferred-path re-record hook (the ledger carry). The
/// `EngineStages` constructor is the production installer — it captures the
/// shared `EpochDelta` and re-records a deferred path's hop-pool keys at the
/// cycle's solve block. Direct engine drives (unit tests, the cold-start
/// `solve_all`) leave it unset, and the deferral falls back to today's
/// dropped behavior.
pub(crate) fn set_deferred_re_record(
    engine: &mut ArbitrageEngine,
    hook: super::DeferredReRecordHook,
) {
    engine.cycle.deferred_re_record = Some(hook);
}
/// Resolve and solve all registered paths (machine-direct cycle route).
/// **Solve-only — does NOT dispatch a batch** (matches the cycle's contract;
/// dispatch is the pump's job via `compute_diff_and_send`, driven by the
/// debounce timer).
///
/// Cold-start / test synchronization entry point (replaces the removed
/// `initial_solve`). Populates `engine.cycle.results` and advances
/// `results_block`; leaves `delivered` untouched (Python has not yet
/// received anything). Callers read results via `latest_results`.
#[tracing::instrument(name = "degenbot.arb.solve_all", skip(engine), fields(block_number, path_count = engine.registry.len()))]
pub(crate) fn solve_all_paths(engine: &mut ArbitrageEngine, block_number: u64) {
    engine.cycle.solve_all_paths(block_number, &engine.registry);
}
/// Number of registered V2 pools (state lives in `BotState` under ADR-003).
#[must_use]
pub(crate) fn v2_pool_count(engine: &ArbitrageEngine) -> usize {
    engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver)
        .v2_pool_count()
}
/// Number of registered V3 pools (state lives in `BotState` under ADR-003).
#[must_use]
pub(crate) fn v3_pool_count(engine: &ArbitrageEngine) -> usize {
    engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver)
        .v3_pool_count()
}
/// Number of registered V4 pools (state lives in `BotState` under ADR-003).
#[must_use]
pub(crate) fn v4_pool_count(engine: &ArbitrageEngine) -> usize {
    engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver)
        .v4_pool_count()
}
/// Number of registered mixed paths.
#[must_use]
pub(crate) fn path_count(engine: &ArbitrageEngine) -> usize {
    engine.registry.len()
}
/// PRG-4 / IRUMXD: the engine path registry owns the registered-path cap (was
/// the Python `MAX_REGISTERED_PATHS` counter). `None` = unlimited. The
/// `PyO3` driver sets it once at boot from the typed config value.
pub(crate) fn set_path_cap(engine: &mut ArbitrageEngine, cap: Option<usize>) {
    engine.registry.set_cap(cap);
}
/// PRG-4: dedup hits counted engine-side — a duplicate registration returns
/// the existing id and never surfaces to the driver as a skip, so the `dup`
/// telemetry needs this witness.
#[must_use]
pub(crate) fn path_dedups(engine: &ArbitrageEngine) -> u64 {
    engine.registry.dedups()
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
    // the detached-solve stance key retired from the schema; there
    // is no opt-out — the one solve arm is unconditional.
}
