//! 5TBT7L T6: test-only free-function harness for the direct-engine drives.
//!
//! The T4 shims (`run_test_cycle`, `merge_detached_for_test`,
//! `finalize_for_test`, `process_updates`) and the YI5NGB boot-stamp probe
//! were inherent `ArbitrageEngine` methods. The one-impl-block structural gate
//! (`just check-engine-impl-blocks`) forbids any inherent engine member outside
//! `arb_engine/mod.rs`, and these carry no composition work — so they live here
//! as free functions over `&ArbitrageEngine` / `&mut ArbitrageEngine`.
//!
//! Every function is `#[cfg(test)]` (the module is declared behind that gate in
//! `mod.rs`), so none of this compiles into a production build.
use super::boot_stamp::BootStamp;
use super::solve_cycle::CycleOutcome;
use super::ArbitrageEngine;
use super::BlockMetadata;
use crate::bot_core::V3SwapUpdate;
use alloy::primitives::aliases::U112;
use alloy::primitives::Address;
use hashbrown::HashSet;

/// YI5NGB test-only F-suite probe: the engine's construction-stamped boot —
/// lets the white-box tests verify twin constructions share a byte-identical
/// boot value WITHOUT reaching into the fleet statics.
pub(crate) fn fleet_boot_stamp(engine: &ArbitrageEngine) -> &BootStamp {
    &engine.fleet_boot_stamp
}

/// 5TBT7L T4 test harness: the direct-engine cycle drive the unit tests used
/// to reach via the retired `ArbitrageEngine::solve_dirty`. Runs the machine's
/// `run_epoch` + the processed-cursor stamp. The PRODUCTION pre-cycle expiry +
/// sidecar spawn live on `EngineStages::run_solve_cycle`, which the stage tests
/// drive directly.
pub(crate) fn run_test_cycle(
    engine: &mut ArbitrageEngine,
    block_number: u64,
    metadata: &BlockMetadata,
    affected: &[degenbot_solvers::affected_keys::AffectedKey],
) -> CycleOutcome {
    let outcome = engine.cycle.run_epoch(
        affected,
        block_number,
        metadata,
        &engine.registry,
        &mut engine.delivery,
    );
    // 6XB6NJ: monotone advance on the block cursor.
    engine.cycle.cursor.advance_processed(block_number);
    outcome
}

/// 5TBT7L T4 test harness: terminal disposition of one detached straggler (the
/// retired `ArbitrageEngine::merge_detached_item`). The production caller is the
/// detached-merge sidecar, which chains machine-direct.
pub(crate) fn merge_detached_for_test(
    engine: &mut ArbitrageEngine,
    item: crate::arb_engine::executor::LaneOutcome,
) {
    engine
        .cycle
        .merge_detached_item(item, &engine.registry, &mut engine.delivery);
}

/// 5TBT7L T4 test harness: the guarded boundary advance + terminal publish (the
/// retired `ArbitrageEngine::finalize_block`, minus the apply-telemetry diag
/// which now rides `EngineStages::on_finalize`).
pub(crate) fn finalize_for_test(
    engine: &mut ArbitrageEngine,
    block: u64,
    metadata: &BlockMetadata,
) {
    if engine.cycle.cursor.finalize(block) {
        super::delivery_policy::compute_diff_and_send(engine, metadata);
    }
}

/// Process pre-decoded updates for testing.
pub(crate) fn process_updates(
    engine: &mut ArbitrageEngine,
    v2_updates: &[(Address, U112, U112)],
    v3_updates: &[V3SwapUpdate],
    block_number: u64,
    metadata: &BlockMetadata,
) {
    // Apply V2+V3 updates to BotState and collect affected pool ids (ADR-003)
    let mut v2_affected = HashSet::new();
    let mut v3_affected = HashSet::new();
    {
        let mut core = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver);
        for &(addr, r0, r1) in v2_updates {
            if let Some(pool_id) = core.apply_v2_sync(addr, r0, r1, block_number) {
                v2_affected.insert(pool_id);
            }
        }
        for update in v3_updates {
            if let Some(pool_id) = core.apply_v3_swap(
                update.pool_address,
                update.sqrt_price_x96,
                update.liquidity,
                update.tick,
                block_number,
                &update.tick_priors,
            ) {
                v3_affected.insert(pool_id);
            }
        }
    }
    // Re-solve only paths containing updated pools (test-only intake)
    engine.cycle.run_epoch(
        &super::tests::test_keys::affected_keys(&v2_affected, &v3_affected, &HashSet::new()),
        block_number,
        metadata,
        &engine.registry,
        &mut engine.delivery,
    );
    // 6XB6NJ: monotone advance on the block cursor.
    engine.cycle.cursor.advance_processed(block_number);
}

/// Test-only read accessor for the block cursor's solved boundary.
pub(crate) fn last_solved_block(engine: &ArbitrageEngine) -> u64 {
    engine.cycle.cursor.last_solved_block()
}

/// Test-only read accessor for the forward-log flag on the block cursor.
pub(crate) fn has_logs_this_block(engine: &ArbitrageEngine) -> bool {
    engine.cycle.cursor.has_logs_this_block()
}

/// Test-only read accessor for the cycle's hop-projection counter.
pub(crate) fn hop_projection_count(engine: &ArbitrageEngine) -> u64 {
    engine.cycle.hop_projection_count
}
