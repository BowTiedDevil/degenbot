use super::*;

// -------------------------------------------------------------------
// QTZGFL: capacity-modulated admission draw (experiment; flag OFF by
// default so the current degrade stays byte-identical).
// -------------------------------------------------------------------
/// Budget arithmetic: `budget = max(0, target − outstanding)` in KEYS,
/// `None` while the stance is OFF, and the target is clamped to the
/// design-locked safety valve. `Some(0)` is the shed predicate.
#[test]
fn admission_budget_arithmetic_and_target_clamp() {
    let (mut engine, _pool_ids, _path_ids) = detached_fixture(0);
    // Stance OFF: no budget — the caller take-alls (byte-identical).
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        None,
        "flag OFF must yield no budget (the take_keys path)"
    );
    engine.cycle.set_solve_admission(true);
    engine.cycle.set_admission_target_depth(3);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(3),
        "empty pipe: full headroom"
    );
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(1, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(2),
        "one straggler: target − 1"
    );
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(3, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(0),
        "at target: zero budget = the SHED verdict"
    );
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(99, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(0),
        "an overshoot saturates at zero (no unsigned wrap)"
    );
    // The target is clamped to the design-locked safety valve.
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(0, std::sync::atomic::Ordering::Relaxed);
    engine.cycle.set_admission_target_depth(usize::MAX);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(
            usize::try_from(crate::arb_engine::detached_cycle::DETACHED_INFLIGHT_CAP)
                .expect("cap fits usize")
        ),
        "an over-cap target clamps to DETACHED_INFLIGHT_CAP"
    );
    engine.cycle.set_admission_target_depth(0);
    assert_eq!(
        engine.cycle.admission_budget_keys(),
        Some(1),
        "a zero target clamps up to 1 (a target of 0 would never submit)"
    );
}
/// Full-path shed: the gauge preloaded AT the target makes the DRAW
/// (the single consumption decision, `on_resolve`) return a zero budget,
/// so the cycle submits NOTHING, advances the solved-block cursor exactly
/// like the `skipped_empty` bookkeeping pass, latches
/// `cycle.arm="shed"`, and counts the shed. Driven through the staged
/// path (Resolved -> Solved), because the dispatch no longer re-reads the
/// gauge — it consumes the draw-time verdict stashed by `on_resolve`.
#[test]
fn admission_zero_budget_sheds_the_whole_cycle() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let (mut engine, pool_ids, path_ids) = detached_fixture(400);
    engine.cycle.set_solve_admission(true);
    engine.cycle.set_admission_target_depth(8);
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    for &p in &pool_ids {
        delta.record(
            degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p),
            10,
        );
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert!(
        drawn.0.is_empty(),
        "a zero-budget draw consumes nothing; the keys are RETAINED"
    );
    let sheds_before = engine
        .lock()
        .cycle
        .detached_cycle
        .shed_cycles
        .load(std::sync::atomic::Ordering::Relaxed);
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    let guard = engine.lock();
    assert_eq!(
        guard.cycle.cycle_arm(),
        "shed",
        "a zero-budget cycle must latch the shed arm"
    );
    assert!(
        path_ids
            .iter()
            .all(|p| !guard.cycle.results.contains_key(p)),
        "a shed cycle SUBMITS NOTHING: no path may be solved or merged"
    );
    assert_eq!(
        guard
            .cycle
            .detached_cycle
            .shed_cycles
            .load(std::sync::atomic::Ordering::Relaxed),
        sheds_before + 1,
        "the shed counter must fire exactly once"
    );
    assert_eq!(
        guard.cycle.cursor.results_block(),
        100,
        "a shed cycle advances the solved-block cursor like skipped_empty"
    );
}
/// F2: a draw-zero shed responds BEFORE the `pending_new_paths` merge, so
/// it never consumes the eager-registration protection — the eagerly
/// solved path is still merged on the NEXT normal cycle (a post-merge
/// shed would clear the pipe and let the next cycle's results replacement
/// drop the eager result).
#[test]
#[expect(clippy::too_many_lines)]
fn admission_draw_zero_shed_preserves_pending_new_paths() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let mut engine = ArbitrageEngine::new();
    let a = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let b = engine.register_v2_pool(
        Address::from([0x12u8; 20]),
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let pid = register_and_solve_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: b,
                zero_for_one: true,
            },
        ],
    )
    .expect("eager path registration succeeds");
    assert!(
        engine.cycle.pending_new_paths.contains(&pid),
        "the eager path starts in the merge pipe"
    );
    engine.cycle.set_solve_admission(true);
    engine.cycle.set_admission_target_depth(8);
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    delta.record(
        degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, a),
        10,
    );
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    // Cycle 1 (draw-zero): the shed responds before the merge.
    let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert!(drawn.0.is_empty(), "zero budget draws nothing");
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    {
        let guard = engine.lock();
        assert_eq!(guard.cycle.cycle_arm(), "shed");
        assert!(
            guard.cycle.pending_new_paths.contains(&pid),
            "a draw-zero shed must NOT consume the eager merge protection"
        );
        assert!(
            guard.cycle.results.contains_key(&pid),
            "the eagerly-solved result survives the shed"
        );
    }
    // Cycle 2 (headroom back): the eager path merges and the pipe clears.
    engine
        .lock()
        .cycle
        .detached_cycle
        .outstanding
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let ctx = BlockContext::new(Epoch::at(11), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    // the cycle enqueues and returns; wait for the sidecar merge.
    // Widened ~15s: the sidecar merge shares solve seats with concurrently
    // running tests; uncontended it lands in ms, but a loaded suite can
    // delay seat acquisition past the old 2s valve.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let merged = {
            let guard = engine.lock();
            guard.cycle.pending_new_paths.is_empty() && guard.cycle.results.contains_key(&pid)
        };
        if merged {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the next normal cycle must merge + clear the eager-registration pipe"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let guard = engine.lock();
    assert!(
        guard.cycle.pending_new_paths.is_empty(),
        "the next normal cycle merges + clears the eager-registration pipe"
    );
    assert!(
        guard.cycle.results.contains_key(&pid),
        "the eager result survives the merge cycle"
    );
}
/// RACE REGRESSION (F1/F3, red-first): the admission budget is decided
/// ONCE, at the DRAW. A cycle that drew a POSITIVE budget owns those keys
/// (they are already removed from the ledger); an earlier cycle's bin
/// thread bumping in-flight to/over the target between the draw and the
/// dispatch must NOT turn that cycle into a shed — the drawn keys would
/// be discarded (never submitted, never re-recorded). WFF6MM: the drawn
/// keys submit down the one (detached) arm regardless of the gauge.
///
/// The test drives both stages explicitly, so it can interleave the
/// in-flight bump exactly in the race window (between `on_resolve` and
/// `on_solve`) — the shape the stage machine's Resolved -> Solved
/// sequencing makes deterministic here. RED on the pre-remediation code
/// (the dispatch re-read the gauge fresh and shed, discarding the drawn
/// keys); GREEN after the draw-time verdict travels with the cycle.
#[test]
fn admission_race_positive_draw_never_sheds() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    engine.cycle.set_solve_admission(true);
    engine.cycle.set_admission_target_depth(8);
    // An eager-registration path in the merge pipe: a post-merge race
    // shed would be the F2 data-loss class.
    engine.cycle.pending_new_paths.insert(path_ids[1]);
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    for &p in &pool_ids {
        delta.record(
            degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p),
            10,
        );
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
    // DRAW with an EMPTY pipe: budget = 8 -> positive; every key drawn.
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert!(
        !drawn.0.is_empty(),
        "an empty pipe must draw a positive budget"
    );
    assert!(
        delta.is_empty(),
        "the draw consumed the ledger keys (a shed would lose them)"
    );
    // RACE: the bin thread bumps in-flight to the cap between the draw
    // and the dispatch.
    engine
        .lock()
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let sheds_before = engine
        .lock()
        .cycle
        .detached_cycle
        .shed_cycles
        .load(std::sync::atomic::Ordering::Relaxed);
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    // the dispatch enqueues and returns; the sidecar merges. Wait
    // for the merge before reading the results/pending pipe.
    // Widened ~15s for the same sidecar seat-contention reason.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let merged = {
            let guard = engine.lock();
            path_ids.iter().all(|p| guard.cycle.results.contains_key(p))
                && guard.cycle.pending_new_paths.is_empty()
        };
        if merged {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sidecar must merge the drawn keys after the detached enqueue"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let guard = engine.lock();
    assert_eq!(
        guard.cycle.cycle_arm(),
        "detached",
        "a positive-budget draw always detaches — there is no in-cycle response left"
    );
    assert_eq!(
        guard
            .cycle
            .detached_cycle
            .shed_cycles
            .load(std::sync::atomic::Ordering::Relaxed),
        sheds_before,
        "a cycle that drew a POSITIVE budget must NEVER shed at the dispatch"
    );
    assert!(
        path_ids.iter().all(|p| guard.cycle.results.contains_key(p)),
        "the drawn keys must be SUBMITTED, never discarded"
    );
    assert!(
        guard.cycle.pending_new_paths.is_empty(),
        "the race cycle must still merge + clear the eager-registration pipe (F2)"
    );
}
/// Carry: a shed cycle RETAINS its keys in the ledger; a later cycle with
/// headroom draws them again (a fresh solve against current state, not a
/// replay). This is the acceptance the whole design turns on.
#[test]
fn admission_carries_retained_keys_to_a_later_cycle() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers};
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let engine = ArbitrageEngine::new();
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    engine.lock().cycle.set_solve_admission(true);
    engine.lock().cycle.set_admission_target_depth(2);
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    let keys: Vec<_> = (1..=3u64)
        .map(|id| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, id))
        .collect();
    for key in &keys {
        delta.record(*key, 10);
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    // Gauge AT the target: zero budget ⇒ shed — nothing drawn, all retained.
    engine
        .lock()
        .cycle
        .detached_cycle
        .outstanding
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert!(
        drawn.0.is_empty(),
        "zero-budget draw takes nothing; the keys are RETAINED"
    );
    assert_eq!(
        delta.snapshot_keys(),
        keys,
        "carry: the ledger keeps every key"
    );
    // Depth falls: the NEXT cycle draws the carried keys (budget = 2).
    engine
        .lock()
        .cycle
        .detached_cycle
        .outstanding
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let ctx = BlockContext::new(Epoch::at(11), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert_eq!(
        drawn.0,
        vec![keys[0], keys[1]],
        "the carried keys are drawn freshest-first in insertion order"
    );
    assert_eq!(
        delta.snapshot_keys(),
        vec![keys[2]],
        "the overflow stays retained for the next cycle"
    );
    // And the final carry drains on the next empty pipe.
    let ctx = BlockContext::new(Epoch::at(12), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert_eq!(drawn.0, vec![keys[2]], "the last carried key drains");
    assert!(delta.is_empty());
}
/// Retention: on a block advance the ledger prunes carried keys older than
/// `head − W` and counts the expiry, so a lead that stays starved
/// eventually expires VISIBLY instead of pinning the ledger forever.
#[test]
fn admission_retention_window_expires_carried_leads() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers};
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let engine = ArbitrageEngine::new();
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    engine.lock().cycle.set_solve_admission(true);
    engine.lock().cycle.set_admission_target_depth(4);
    engine.lock().cycle.set_admission_retention_blocks(5);
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
    let stale = degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, 1);
    let fresh = degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, 2);
    delta.record(stale, 10);
    delta.record(fresh, 100);
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    // head 100, W 5 ⇒ cutoff 95: the block-10 lead expires; the block-100
    // lead is drawn.
    let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert_eq!(drawn.0, vec![fresh], "only the in-window lead is drawn");
    assert_eq!(
        engine
            .lock()
            .cycle
            .detached_cycle
            .leads_expired
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the retention prune must count exactly one expired lead"
    );
    assert!(delta.is_empty());
}
/// Flag OFF: `on_resolve` take-alls (even with the gauge saturated) and
/// the engine DETACHES every cycle — never a shed, and never any
/// in-cycle degrade either.
#[test]
fn admission_off_keeps_take_all_and_never_sheds() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers};
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
    // Stance left OFF; the gauge at the cap must NOT shed.
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    assert_eq!(
        engine.cycle.cycle_arm(),
        "detached",
        "flag OFF: the cycle still takes the one dispatch arm"
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .shed_cycles
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "flag OFF must never shed"
    );
    // And the draw path take-alls regardless of the gauge.
    let engine_arc = Arc::new(parking_lot::Mutex::new(engine));
    let delta = Arc::new(EpochDelta::new(0u64));
    let stages = EngineStages::new(Arc::clone(&engine_arc), Arc::clone(&delta));
    for id in 1..=5u64 {
        delta.record(
            degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, id),
            10,
        );
    }
    let quiesced = QuiesceOutcome {
        verdict: QuiesceVerdict::Settled,
    };
    let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    assert_eq!(
        drawn.0.len(),
        5,
        "flag OFF: take_keys consumes the whole ledger"
    );
    assert!(delta.is_empty());
}
/// N1 (in-cycle duplicate policy, tightened): a duplicate
/// (`solve_seq`, pid) arrival at the in-cycle drain must be REFUSED —
/// counted and logged, never merged twice. Red at HEAD against the
/// NEW policy (today the in-cycle drain logs-and-merges anyway —
/// sD:2550 — because its local set is dropped with the cycle).
// pins the tightened refuse-the-merge policy
// (design §4.4 REV 2 decision, Risk 5 option 1).
#[test]
fn duplicate_lane_outcome_does_not_double_apply() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    let pid = path_ids[0];
    let results_before = engine.cycle.results.len();
    let applied_before = engine
        .cycle
        .detached_cycle
        .applied
        .load(std::sync::atomic::Ordering::Relaxed);
    // the machine issues the seq (no in-cycle counter) — read
    // back the tick the cycle actually claimed so the replay collides.
    let cycle_seq = engine.cycle.detached_cycle.issued_seq();
    // A second Solved arrival for the SAME (seq, pid) through the merge
    // disposition must be refused: no second results write, no applied
    // count for the duplicate.
    let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
    let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
    let item = crate::arb_engine::executor::LaneOutcome::Solved(
        crate::arb_engine::executor::SolveOutcome {
            payload: None,
            worker_clamp_twins: 0,
            cycle_seq,
            solve_block: 100,
            metadata: BlockMetadata::default(),
            pid,
            update_stamp: fresh_stamp,
            result: fresh_result,
            solve_span: tracing::Span::none(),
        },
    );
    merge_detached_for_test(&mut engine, item);
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .duplicate_outcomes
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "in-cycle dup policy (tightened): the fuse must trip exactly once"
    );
    assert_eq!(
        engine.cycle.results.len(),
        results_before,
        "in-cycle dup policy (tightened): the refused duplicate must not re-merge"
    );
    let _ = applied_before;
}
#[test]
#[expect(clippy::too_many_lines)] // A/B harness: two full engines, worth the length
fn resolve_chunk_parity_parallel_matches_serial_and_reuses_cache_walks() {
    const N: usize = 600; // >= RESOLVE_PAR_MIN (512) so the parallel arm engages
    let build = || {
        let mut engine = ArbitrageEngine::new();
        // Two HUB pools shared by every path; one unique pool per path.
        let hub_a = engine.register_v2_pool(
            Address::from([0xaa_u8; 20]),
            usdc(1_000_000),
            weth(700),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let hub_b = engine.register_v2_pool(
            Address::from([0xbb_u8; 20]),
            weth(900),
            usdc(1_200_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let mut path_ids = Vec::with_capacity(N);
        for i in 0..N {
            let mut b = [0x33u8; 20];
            // Distinct for the whole 0..600 range: high byte of the
            // 16-bit index in b[15], low byte in b[16] (expect: a test
            // address space, values provably < 256 per byte).
            b[15] = u8::try_from(i / 256).expect("N < 65536");
            b[16] = u8::try_from(i % 256).expect("index mod 256 fits u8");
            b[17] = 0x5au8;
            let unique = engine.register_v2_pool(
                Address::from(b),
                usdc(50_000),
                weth(30),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let id = register_and_solve_path(
                &mut engine,
                vec![
                    PoolHop {
                        pool_id: hub_a,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: unique,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: hub_b,
                        zero_for_one: false,
                    },
                ],
            )
            .unwrap();
            path_ids.push((id, unique, hub_a, hub_b));
        }
        (engine, path_ids, hub_a, hub_b)
    };
    let run = |parallel: bool| {
        let (mut engine, path_ids, hub_a, hub_b) = build();
        // the A/B arm drives the INSTANCE stance now (no
        // process-global flip; no parallel-order dependence).
        engine.cycle.set_resolve_parallel_for_test(parallel);
        // Cycle 1: dirty BOTH hubs -> all N paths re-resolve in one cycle.
        process_updates(
            &mut engine,
            &[
                (Address::from([0xaa_u8; 20]), usdc(990_000), weth(705)),
                (Address::from([0xbb_u8; 20]), weth(895), usdc(1_210_000)),
            ],
            &[],
            500,
            &BlockMetadata::default(),
        );
        engine.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &HashSet::from([hub_a, hub_b]),
                &HashSet::new(),
                &HashSet::new(),
            ),
            500,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
        // Cycle 2: dirty hub_b only -> 600 affected paths again; hub_a must
        // be walked ONCE by the shared sharded cache (serial: also once).
        let projections_before = engine.cycle.hop_projection_count;
        process_updates(
            &mut engine,
            &[(Address::from([0xbb_u8; 20]), weth(880), usdc(1_230_000))],
            &[],
            501,
            &BlockMetadata::default(),
        );
        engine.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &HashSet::from([hub_b]),
                &HashSet::new(),
                &HashSet::new(),
            ),
            501,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
        let projections_delta = engine.cycle.hop_projection_count - projections_before;
        let (results, _block) = latest_results(&engine);
        (
            results,
            engine.cycle.paths_same_state_this_cycle,
            projections_delta,
            path_ids,
        )
    };
    let (serial_results, serial_same_state, serial_proj_delta, path_ids) = run(false);
    let (par_results, par_same_state, par_proj_delta, _path_ids) = run(true);
    // the instance-stance cutover -> nothing process-global remains to restore.
    assert_eq!(path_ids.len(), N);
    for (path_id, _unique, _a, _b) in &path_ids {
        let sres = serial_results.get(path_id).expect("serial result");
        let pres = par_results.get(path_id).expect("parallel result");
        assert_eq!(
            sres.profit, pres.profit,
            "profit diverged for path {path_id}"
        );
        assert_eq!(
            sres.solver_pool_states.len(),
            pres.solver_pool_states.len(),
            "hop-state shape diverged for path {path_id}"
        );
    }
    assert_eq!(
        serial_same_state, par_same_state,
        "same-state accounting diverged"
    );
    // THE cache-reuse invariant: both arms walk the same pool count on the
    // second cycle, and exactly one walk per distinct pool (not per chunk).
    assert_eq!(
        serial_proj_delta, par_proj_delta,
        "projection walks diverged"
    );
    // Cycle-2 re-resolve of 600 paths: only hub_b (dirty) misses the cache;
    // hub_a hits in every path. 1 walk total in BOTH arms.
    assert_eq!(
        serial_proj_delta, 1,
        "cycle-2 must walk only the dirty pool, got {serial_proj_delta}"
    );
}
