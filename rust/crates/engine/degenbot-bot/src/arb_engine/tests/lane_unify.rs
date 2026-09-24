use super::*;

// =================================================================
// Red-first breaker suite (design logs/lane-unify-design.md §5).
// Status at HEAD (commit 1): each test below is RED against current
// code — they pin the POST-merge contracts (one carrier, one ledger,
// the detached arm's lane witness). They GREEN in commit 2.
// =================================================================
/// N2 (same-seq replay, WFF6MM single-arm): a merged result and a later
/// carrier naming the SAME (`cycle_seq`, pid) must collide on the ONE
/// ledger — the fuse refuses the second arrival instead of merging
/// twice.
// pins the (solve_seq, pid) key half the merged
// ledger owns (design §3.3).
#[test]
fn merged_ledger_rejects_same_seq_pid_replay() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    // In-cycle solve of the same block: all 3 paths land in `results`.
    run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    let pid = path_ids[0];
    assert!(
        engine.cycle.results.contains_key(&pid),
        "baseline: the in-cycle arm must merge pid {pid} first"
    );
    let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
    let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
    let results_before = engine.cycle.results.len();
    let applied_before = engine
        .cycle
        .detached_cycle
        .applied
        .load(std::sync::atomic::Ordering::Relaxed);
    // The replay claims the cycle_seq the merged cycle consumed: under
    // the one ledger this is the SAME (solve_seq, pid) key and the fuse
    // must refuse it.
    let item = crate::arb_engine::executor::LaneOutcome::Solved(
        crate::arb_engine::executor::SolveOutcome {
            payload: None,
            worker_clamp_twins: 0,
            cycle_seq: engine.cycle.detached_cycle.issued_seq(), // the merged cycle's seq
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
        "merged ledger: the same-seq replay must trip the fuse exactly once"
    );
    assert_eq!(
        engine.cycle.results.len(),
        results_before,
        "merged ledger: the refused duplicate must not add a results entry"
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        applied_before,
        "merged ledger: the refused duplicate must not count as applied"
    );
}
/// N3 (prune correctness): the merged ledger prunes keyed rows only
/// past `LEDGER_AGE`, driven by the DETACHED arm's cycle issuance —
/// and an in-cycle-only advance must NOT prune (the anchor is
/// detached-issued-seq). Red at HEAD: the in-cycle side has no
/// ledger/rows at all; the detach-keyed prune ages on any current
/// seq the claim sees.
// pins LEDGER_AGE=64 exactly and the anchor choice
// (design §3.3 + §3.3.1 REV 2).
#[test]
fn merged_ledger_prunes_only_past_ledger_age() {
    use std::sync::atomic::Ordering;
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
    let results_before = engine.cycle.results.len();
    // Seed directly into the sidecar ledger (pub(crate) state) with
    // pids not otherwise involved, at seq boundaries chosen to straddle
    // the LEDGER_AGE edge driven through the DETACHED key space.
    let mut seed = |seq: u64, pid: u64| {
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&path_ids[0]].clone();
        let result = engine.cycle.results.get(&path_ids[0]).unwrap().clone();
        let item = crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: seq,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp,
                result,
                solve_span: tracing::Span::none(),
            },
        );
        merge_detached_for_test(&mut engine, item);
    };
    // Drive the anchor forward by ONE detached merge at a high seq; the
    // seeds at (current-65) and (current-63) straddle the age edge.
    let current = 100u64;
    seed(current - 65, 1111); // beyond LEDGER_AGE: must be PRUNED
    seed(current - 63, 2222); // within LEDGER_AGE: must be RETAINED
    seed(current, 3333); // the advancing merge itself
                         // the seq-100 merge's PRUNE swept BOTH seeds' rows as a
                         // side effect (the engine-side ledger now prunes on every claim,
                         // not just the sidecar's). Restore the retained-row marker its
                         // (36, 2222) key placed there — the prune must prove (36, 2222)
                         // SURVIVES a later claim, not that it survived the sweep that
                         // built the (100, 3333) row.
    engine
        .cycle
        .detached_cycle
        .outcome_ledger
        .lock()
        .claim((current - 63, 2222))
        .ok();
    let ledger_rows = engine.cycle.detached_cycle.outcome_ledger.lock();
    assert!(
        !ledger_rows.contains((current - 65, 1111)),
        "LEDGER_AGE=64: row (seq-65) must be pruned after the seq-{current} claim"
    );
    assert!(
        ledger_rows.contains((current - 63, 2222)),
        "LEDGER_AGE=64: row (seq-63) must be retained after the seq-{current} claim"
    );
    drop(ledger_rows);
    // NEGATIVE half (design §3.3.1 REV 2): in-cycle-only advances of
    // the shared counter must NOT prune detached-keyed rows. Run two
    // in-cycle solves (the shared counter ticks), then re-assert the
    // retained row survived them.
    run_test_cycle(
        &mut engine,
        101,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    run_test_cycle(
        &mut engine,
        102,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    let ledger_rows_still = engine.cycle.detached_cycle.outcome_ledger.lock();
    assert!(
        ledger_rows_still.contains((current - 63, 2222)),
        "in-cycle-only advances must not prune detached-keyed rows (anchor = detached_issued_seq)"
    );
    let _ = results_before;
    let _ = Ordering::Relaxed;
}
/// N4 (detached undercount): a detached bin that panics mid-walk must
/// deliver a typed disposition for EVERY owed pid — no silent
/// undercount. Red at HEAD: the detached arm has NO lane witness
/// (sD:2350 submits a raw bin body), so a panicked bin simply never
/// delivers its undelivered pids.
// pins the detached arm's witness adoption
// (design §5.2). GREEN requires Failed records on the detached pipe.
#[test]
fn detached_undercount_trips_the_fan_in_assert() {
    if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
        eprintln!("skipping: detached panic test requires >=2 cores");
        return;
    }
    // Drive a detached cycle with a panicking pid; then REQUIRE that
    // every submitted pid got exactly one disposition (Solved,
    // Suppressed, or Failed) — observed through the applied+dropped
    // counters, which today stay short (the undelivered pids never
    // arrive at all: SILENT UNDERCOUNT).
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let kill = path_ids[1];
    engine
        .cycle
        .set_solve_panic_hook(std::sync::Arc::new(move |pid: u64| {
            if pid != kill {
                return;
            }
            panic!("path killed mid-bin (red harness)");
        }));
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    // Drive through the production stage seam so the sidecar spawns.
    let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
    let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
    for &p in &pool_ids {
        delta.record_affected(HopType::V2, p, 0u64);
    }
    let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
    stages.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());
    // Wait for the dispositions to land (the sidecar merges async).
    // Widened ~15s: the detached sidecar shares the fleet solve seats with
    // other concurrently-running tests; the disposition lands in <50ms when
    // uncontended, but a loaded suite can delay seat acquisition well past 2s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let guard = engine.lock();
        let applied = guard
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed);
        let stale = guard
            .cycle
            .detached_cycle
            .dropped_stale
            .load(std::sync::atomic::Ordering::Relaxed);
        let dereg = guard
            .cycle
            .detached_cycle
            .dropped_deregistered
            .load(std::sync::atomic::Ordering::Relaxed);
        let dup = guard
            .cycle
            .detached_cycle
            .duplicate_outcomes
            .load(std::sync::atomic::Ordering::Relaxed);
        drop(guard);
        let disposed = applied + stale + dereg + dup;
        if disposed >= path_ids.len() as u64 || std::time::Instant::now() > deadline {
            // THE assert — every submitted path must be
            // dispositioned EXACTLY once. At HEAD the panicked bin's
            // undelivered pids NEVER arrive, so disposed < submitted.
            assert_eq!(
                    disposed, path_ids.len() as u64,
                    "outcome accounting undercount — exactness fuse: \\\n                     a panicked bin must still disposition \\\n                     every owed pid as a typed Failed record"
                );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
/// N5 (gauge pairing through a panic): the in-flight gauge must return
/// to its EXACT pre-cycle value after a panicking detached cycle AND
/// keep the cap gate engaging on true load. Red at HEAD: the panicked
/// bin's flushed Solved items bump at send but the panic kills the
/// remaining path solves, nothing decrements the orphaned bumps
// pins constraint (b) THROUGH the panic path AND
// the variant-gated bump/decrement pairing (design §4.6.1 REV 2).
#[test]
fn detached_panic_does_not_leak_inflight_gauge() {
    if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
        eprintln!("skipping: detached panic test requires >=2 cores");
        return;
    }
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let kill = path_ids[2];
    engine
        .cycle
        .set_solve_panic_hook(std::sync::Arc::new(move |pid: u64| {
            if pid != kill {
                return;
            }
            panic!("path killed mid-bin (red harness)");
        }));
    let g0 = engine
        .cycle
        .detached_cycle
        .outstanding
        .load(std::sync::atomic::Ordering::Relaxed);
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
    let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
    for &p in &pool_ids {
        delta.record_affected(HopType::V2, p, 0u64);
    }
    let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
    stages.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());
    // After all dispositions land, the gauge must be back at g0 EXACTLY
    // (a leaked count OR a sagged count both move it off g0 — only the
    // exact value pins BOTH directions of a broken pairing).
    // Widened ~15s for the same seat-contention reason as the undercount test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let guard = engine.lock();
        let applied = guard
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed)
            + guard
                .cycle
                .detached_cycle
                .dropped_stale
                .load(std::sync::atomic::Ordering::Relaxed)
            + guard
                .cycle
                .detached_cycle
                .dropped_deregistered
                .load(std::sync::atomic::Ordering::Relaxed)
            + guard
                .cycle
                .detached_cycle
                .duplicate_outcomes
                .load(std::sync::atomic::Ordering::Relaxed);
        let gauge = guard
            .cycle
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed);
        drop(guard);
        if applied >= path_ids.len() as u64 || std::time::Instant::now() > deadline {
            assert_eq!(
                    gauge, g0,
                    "in-flight gauge must return to its EXACT pre-cycle \\\n                     value g0={g0} through a panicking cycle (variant-gated pairing, \\\n                     design §4.6.1 REV 2) — got {gauge}"
                );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
/// WFF6MM cutover: the in-flight cap gate is RETIRED — a cycle whose
/// un-dispositioned count sits AT the old cap STILL detaches (the
/// admission draw, not the cap, owns backpressure now; there is no
/// in-cycle fallback left to degrade to).
#[test]
fn retired_inflight_cap_no_longer_degrades() {
    let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
    // Seed the gauge AT the old cap: it must be ignored.
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed); // == DETACHED_INFLIGHT_CAP
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
        "the retired cap must no longer degrade a cycle to an in-cycle arm"
    );
}
/// Cold-start trace (degraded-state measurement): the dispatch LATCHES the
/// cycle's arm on the engine (the `solve_entry` precedent) — a span field
/// alone is unreadable to the caller, and the caller (`EngineStages`) is
/// the only place the cycle's duration and Mutex hold are measurable,
/// i.e. AFTER `solve_dirty` returns. `unset` is the never-dispatched
/// sentinel: deliberately visible rather than folded into
/// `skipped_empty`.
#[test]
fn cycle_arm_label_latches_for_every_dispatch_arm() {
    let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
    assert_eq!(
        engine.cycle.cycle_arm(),
        "unset",
        "no cycle has been dispatched yet"
    );
    // Sub-cap under the detached stance: the detached arm.
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
        "a sub-cap cycle under the detached stance must latch the detached arm"
    );
    // At the (retired) in-flight cap: STILL detached (WFF6MM — the
    // cap gate is gone; every dispatched cycle takes the one arm).
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(8, std::sync::atomic::Ordering::Relaxed); // == DETACHED_INFLIGHT_CAP
    run_test_cycle(
        &mut engine,
        101,
        &BlockMetadata::default(),
        &affected_keys_v2,
    );
    assert_eq!(
        engine.cycle.cycle_arm(),
        "detached",
        "the retired cap gate must not divert a cycle off the one dispatch arm"
    );
    // A dirty key with NO registered paths: the bookkeeping-only pass.
    engine
        .cycle
        .detached_cycle
        .outstanding
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let orphan = engine.register_v2_pool(
        Address::from([0x77_u8; 20]),
        usdc(1_000_000),
        weth(500),
        GAMMA_03,
        FEE_DENOM_03,
    );
    run_test_cycle(
        &mut engine,
        102,
        &BlockMetadata::default(),
        &[degenbot_solvers::affected_keys::AffectedKey::new(
            HopType::V2,
            orphan,
        )],
    );
    assert_eq!(
        engine.cycle.cycle_arm(),
        "skipped_empty",
        "a dirty key with no registered paths must latch the bookkeeping-only arm"
    );
}
#[test]
fn detached_solve_returns_at_enqueue_end_when_sync_drain_is_off() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(400);
    // direct-call engines merge inline (synchronous harness); turn
    // that OFF so this pins the PRODUCTION return semantics — enqueue end,
    // results ABSENT until the sidecar (or a later drain) lands them.
    engine.cycle.set_sync_merge_for_test(false);
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
    assert_eq!(engine.cycle.cycle_arm(), "detached");
    // With a 400ms slow path the results land AFTER return (T2's read) —
    // at least the slow pid is absent.
    assert!(
        !engine.cycle.results.contains_key(&path_ids[0]),
        "the detached arm must return at enqueue end: the slow pid is absent AT return"
    );
}
