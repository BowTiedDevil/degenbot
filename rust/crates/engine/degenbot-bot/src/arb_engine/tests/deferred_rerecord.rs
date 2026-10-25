use super::*;
use crate::arb_engine::solve_cycle::{CycleArm, CycleOutcome};

// the deferred-path carry — the ledger re-record at the deferral
// site. The future-price tripwire is unreachable after the solve-anchor
// head floor (only a mid-solve state advance can trip it), so these tests
// install the `force_deferred` construction-time stance to exercise the
// carry deterministically. The re-record hook itself is the production seam
// the `EngineStages` constructor installs (`set_deferred_re_record`);
// with the hook unset (direct engine drives) the deferral keeps today's
// drop.
// -------------------------------------------------------------------
fn kjwik5_key(pool_id: u64) -> degenbot_pools::solve::affected::AffectedKey {
    degenbot_pools::solve::affected::AffectedKey::new(HopType::V2, pool_id)
}
fn kjwik5_affected_keys(pool_ids: &[u64]) -> Vec<degenbot_pools::solve::affected::AffectedKey> {
    pool_ids.iter().copied().map(kjwik5_key).collect()
}
fn kjwik5_path_keys(
    engine: &ArbitrageEngine,
    path_id: u64,
) -> Vec<degenbot_pools::solve::affected::AffectedKey> {
    engine
        .path_pools()
        .get(&path_id)
        .expect("registered path")
        .pools
        .iter()
        .map(|r| degenbot_pools::solve::affected::AffectedKey::new(r.hop_type, r.pool_key))
        .collect()
}

/// The deferral count the typed cycle arm carries — the same census the
/// `[solve-phase]` telemetry renders as `paths.deferred_future_price`.
fn kjwik5_deferred_future_price(outcome: &CycleOutcome) -> usize {
    match outcome.arm {
        CycleArm::Solved {
            deferred_future_price,
            ..
        }
        | CycleArm::Dissolved {
            deferred_future_price,
            ..
        } => deferred_future_price,
        CycleArm::Shed { .. } | CycleArm::SkippedEmpty { .. } => 0,
    }
}

/// Red-first (d): with the hook unset (direct engine drive, no driver),
/// the deferral keeps today's dropped behavior — the deferred path is not
/// submitted, its siblings still solve, and the typed arm reports the
/// deferral count.
#[test]
fn deferred_path_without_hook_keeps_today_drop_behavior() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[1];
    engine.cycle.force_deferred = Some(HashSet::from([deferred]));
    let outcome = run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &kjwik5_affected_keys(&pool_ids),
    );
    assert_eq!(
        kjwik5_deferred_future_price(&outcome),
        1,
        "the typed arm carries the deferral count"
    );
    let (results, _block) = latest_results(&engine);
    assert!(
        !results.contains_key(&deferred),
        "with the hook unset the deferred path keeps today's dropped behavior"
    );
    assert!(results.contains_key(&path_ids[0]));
    assert!(results.contains_key(&path_ids[2]));
}

/// Red-first (c): the deferral count is reported on the typed cycle arm —
/// 0 with no forced deferral, N with N.
#[test]
fn future_price_counter_semantics_are_unchanged() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let affected = kjwik5_affected_keys(&pool_ids);
    let baseline = run_test_cycle(&mut engine, 100, &BlockMetadata::default(), &affected);
    assert_eq!(
        kjwik5_deferred_future_price(&baseline),
        0,
        "no future-priced hop → the arm reports 0"
    );
    engine.cycle.force_deferred = Some(HashSet::from([path_ids[0], path_ids[2]]));
    let forced = run_test_cycle(&mut engine, 101, &BlockMetadata::default(), &affected);
    assert_eq!(
        kjwik5_deferred_future_price(&forced),
        2,
        "two deferred paths count on the same typed arm"
    );
}

/// Red-first (b): the staged carry. Cycle 1 defers the future-priced path,
/// re-records ALL its hop pools into the ledger, and cycle 2 (the anchor
/// caught up) draws it back and solves it against the retry cycle's block.
#[test]
fn staged_deferred_path_carries_via_the_ledger_and_solves_on_the_retry() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use degenbot_substrate::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[0];
    let deferred_keys = kjwik5_path_keys(&engine, deferred);
    engine.cycle.force_deferred = Some(HashSet::from([deferred]));
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    for &p in &pool_ids {
        delta.record(kjwik5_key(p), 10);
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    // Cycle 1: draw, defer, re-record into the ledger at block 100.
    let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert_eq!(
        drawn.with_ticket(|t| t.affected().len()),
        pool_ids.len(),
        "cycle 1 draws every seed key"
    );
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    assert!(
        !latest_results(&engine.lock()).0.contains_key(&deferred),
        "cycle 1 defers the path (it is never submitted)"
    );
    let pending = delta.snapshot_keys();
    for key in &deferred_keys {
        assert!(
            pending.contains(key),
            "the deferred path's key {key:?} is re-recorded for the next draw"
        );
    }
    // The anchor catches up: the path is no longer future-priced. Cycle 2
    // draws it back from the ledger.
    engine.lock().cycle.force_deferred = None;
    let ctx = BlockContext::new(Epoch::at(101), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    let drawn_keys: HashSet<_> = drawn
        .with_ticket(|t| t.affected().to_vec())
        .into_iter()
        .collect();
    for key in &deferred_keys {
        assert!(
            drawn_keys.contains(key),
            "the retry draw re-includes {key:?}"
        );
    }
    let solved = stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    // Q1a: the retried path's solve block is the RETRY cycle's block.
    assert_eq!(
        solved.solved.block(),
        101,
        "the retried path solves at the retry cycle's block"
    );
    // The retried path merges through the sidecar (asynchronous).
    // Widened ~15s: the sidecar merge shares solve seats with concurrently
    // running tests, so the old 5s valve could expire under load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !latest_results(&engine.lock()).0.contains_key(&deferred) {
        assert!(
            std::time::Instant::now() < deadline,
            "the retried path must solve and merge via the sidecar"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Guard 4: the retention window now bounds the deferred-retry lifetime —
/// a deferred lead never redrawn within `admission_retention_blocks` is
/// pruned and counted in `degenbot.detached.leads_expired`.
#[test]
fn staged_deferred_retry_expires_after_the_retention_window() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use degenbot_substrate::{BlockContext, Epoch, EpochDelta};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[0];
    let deferred_keys = kjwik5_path_keys(&engine, deferred);
    engine.cycle.set_solve_admission(true);
    engine.cycle.set_admission_target_depth(8);
    // Cycle 1's window is wide (nothing seeded may expire before the
    // deferral); it is narrowed before cycle 2 to bound the retry.
    engine.cycle.set_admission_retention_blocks(200);
    engine.cycle.force_deferred = Some(HashSet::from([deferred]));
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    for &p in &pool_ids {
        delta.record(kjwik5_key(p), 10);
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    // Cycle 1 defers + re-records the deferred path at block 100.
    let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    // The typed Solved outcome carries the anchor the deferral was
    // re-recorded against.
    let defer_block = stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible")
        .solved
        .block();
    let expired_before = engine
        .lock()
        .cycle
        .detached_cycle
        .leads_expired
        .load(Ordering::Relaxed);
    // Cycle 2: narrow the window to zero and advance one block — the
    // cutoff prunes the deferred lead's bucket before the draw, so it is
    // never redrawn.
    engine.lock().cycle.force_deferred = None;
    engine.lock().cycle.set_admission_retention_blocks(0);
    let ctx = BlockContext::new(Epoch::at(defer_block + 1), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert!(
        !drawn.with_ticket(|t| t.affected().iter().any(|key| deferred_keys.contains(key))),
        "the deferred retry lead expired at the retention window boundary"
    );
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    let expired_after = engine
        .lock()
        .cycle
        .detached_cycle
        .leads_expired
        .load(Ordering::Relaxed);
    assert_eq!(
        expired_after - expired_before,
        u64::try_from(deferred_keys.len()).expect("small key count"),
        "the expired deferred lead is counted (degenbot_detached_leads_expired_total)"
    );
}
