use super::*;

// -------------------------------------------------------------------
// detached enqueue + sidecar merge
// -------------------------------------------------------------------
/// Structural acceptance (red/green): with the one (detached) arm and an
/// injected 3000ms slow path, `run_epoch` — driven via
/// the production `EngineStages` solve seam, which also spawns the
/// merge sidecar — RETURNS before the merge lands, and the sidecar
/// populates every result (slow + fast) shortly after enqueue.
#[test]
fn detached_cycle_returns_at_enqueue_end_and_sidecar_merges() {
    if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
        eprintln!("skipping: detached-cycle structural test requires >=2 cores");
        return;
    }
    let (engine, pool_ids, path_ids) = detached_fixture(3000);
    let slow_pid = path_ids[0];
    let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    let handle = crate::arb_engine::EngineStages::new(
        std::sync::Arc::clone(&engine),
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    let t0 = std::time::Instant::now();
    handle.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());
    let returned = t0.elapsed();
    // RETURNS before the merge lands: strictly inside the injected 3000ms
    // slow-solve window, and the slow path's result is NOT in the map yet.
    assert!(
        returned < std::time::Duration::from_millis(2500),
        "detached cycle must return at enqueue end (before the 3000ms slow \
             solve can merge); took {returned:?}"
    );
    {
        let engine_guard = engine.lock();
        assert!(
            !engine_guard.cycle.results.contains_key(&slow_pid),
            "the slow path must NOT be merged at enqueue-end return"
        );
    }
    // The sidecar populates every result (fast + slow) shortly after
    // enqueue. Wait for the FULL set, not just the slow pid: the fast
    // stragglers merge on the same sidecar pipe and their completion order
    // relative to the slow pid is not guaranteed, so a slow-pid-only wait
    // could read `results.len()` while a fast merge was still in flight and
    // flaked at 2 != 3. Widened ~15s: the sidecar shares solve seats with
    // concurrently running tests, so the old 5s valve could expire under load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while engine.lock().cycle.results.len() < 3 {
        assert!(
            std::time::Instant::now() < deadline,
            "the sidecar must merge all three detached stragglers within ~15s of enqueue"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The slow (delayed) path is among them — proven applied by the sidecar,
    // not the inline drain (it was absent at enqueue-end return above).
    assert!(
        engine.lock().cycle.results.contains_key(&slow_pid),
        "the slow path must be applied by the sidecar"
    );
    let guard = engine.lock();
    assert_eq!(
        guard
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        3
    );
}
/// Q1a stale policy (red/green): a straggler whose resolved-update stamp
/// is stale (a pool ticked during the solve → its `update_block` moved)
/// is DROPPED, not applied. A fresh-stamp straggler applies
/// (apply-if-unchanged) through the same merge seam. LW-T9: the two
/// dispositions ride DIFFERENT pids — the exactness fuse (carried
/// to the sidecar) owns per-(cycle_seq, pid) delivery uniqueness, so the
/// stale→fresh stamp flip on ONE pid is fused by construction now.
#[test]
fn detached_straggler_with_stale_update_stamp_is_dropped() {
    // Const hoist: the straggler probes a seq the baseline
    // in-cycle run did NOT claim (that run consumed seq 1; the ONE
    // (`solve_seq`, pid) ledger must not false-trip a plain sidecar
    // Q1a probe).
    const STRAGGLER_SEQ: u64 = 2;
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
    let stale_pid = path_ids[0];
    let fresh_pid = path_ids[1];
    assert!(
        engine.cycle.results.contains_key(&stale_pid)
            && engine.cycle.results.contains_key(&fresh_pid),
        "precondition: fresh results merged by the inline drain"
    );
    // the baseline cycle's own merges counted here — the straggler
    // assertions below are DELTAS against this snapshot.
    let applied_before = engine
        .cycle
        .detached_cycle
        .applied
        .load(std::sync::atomic::Ordering::Relaxed);
    let stale_stamp: Vec<u64> = engine.cycle.resolved_update_snapshot[&stale_pid]
        .clone()
        .iter()
        .map(|b| b + 1)
        .collect();
    let stale_result = engine.cycle.results.get(&stale_pid).unwrap().clone();
    let fresh_stamp = engine.cycle.resolved_update_snapshot[&fresh_pid].clone();
    let fresh_result = engine.cycle.results.get(&fresh_pid).unwrap().clone();
    let item = |result: SolvePathResult, stamp: Vec<u64>, pid: u64| {
        crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: STRAGGLER_SEQ,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: stamp,
                result,
                solve_span: tracing::Span::none(),
            },
        )
    };
    // A straggler whose pools ALL ticked during the solve.
    merge_detached_for_test(&mut engine, item(stale_result, stale_stamp, stale_pid));
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .dropped_stale
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the stale straggler must be dropped"
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        applied_before
    );
    // The unchanged-intake twin APPLIES (apply-if-unchanged).
    merge_detached_for_test(&mut engine, item(fresh_result, fresh_stamp, fresh_pid));
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        applied_before + 1,
        "the unchanged straggler must be applied"
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .dropped_stale
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
/// LW-T9 note-(a) carry (red): the DETACHED sidecar merge must carry the
/// SAME exactness assert as the in-cycle drain: one path outcome
/// exactly once — a duplicate (`cycle_seq`, `pid`) delivery trips the loud
/// exactness fuse and is NOT applied a second time. RED before the
/// cutover: the sidecar had no duplicate guard, so the twin merge
/// counted applied == 2.
#[test]
fn detached_duplicate_straggler_trips_the_exactness_fuse() {
    // Const hoist: the baseline in-cycle run consumed seq 1 — the
    // straggler probes the NEXT seq so it cannot shadow a key the
    // in-cycle arm already claimed under the ONE (`solve_seq`, pid)
    // ledger (that collision is N2's job; R1 pins a SIDECAR-ONLY
    // double-delivery).
    const STRAGGLER_SEQ: u64 = 2;
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
    let applied_before = engine
        .cycle
        .detached_cycle
        .applied
        .load(std::sync::atomic::Ordering::Relaxed);
    let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
    let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
    let item = |result: SolvePathResult| {
        crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: STRAGGLER_SEQ,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp.clone(),
                result,
                solve_span: tracing::Span::none(),
            },
        )
    };
    merge_detached_for_test(&mut engine, item(fresh_result.clone()));
    merge_detached_for_test(&mut engine, item(fresh_result));
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        applied_before + 1,
        "the duplicate delivery must not apply a second time"
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .duplicate_outcomes
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the duplicate delivery must trip the loud exactness fuse once"
    );
}
/// RLVDUP T3 (red/green): de-registration removes the resolve
/// bookkeeping - `path_status` and `resolved_update_snapshot` entries
/// must follow the path out, or per-pool churn grows the maps
/// unbounded.
#[test]
fn deregister_removes_status_and_update_snapshot() {
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
    assert!(engine.cycle.resolved_update_snapshot.contains_key(&pid));
    assert!(engine.cycle.path_status.contains_key(&pid));
    assert!(deregister_path(&mut engine, pid));
    assert!(!engine.cycle.resolved_update_snapshot.contains_key(&pid));
    assert!(!engine.cycle.path_status.contains_key(&pid));
}
/// Q1a deregister (red/green): a straggler landing after its path was
/// de-registered is DROPPED, never applied (and never re-creates a
/// result entry).
#[test]
fn detached_straggler_after_deregister_is_dropped() {
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
    let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
    let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
    assert!(deregister_path(&mut engine, pid), "path must deregister");
    assert!(!engine.cycle.results.contains_key(&pid));
    merge_detached_for_test(
        &mut engine,
        crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: 1,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp,
                result: fresh_result,
                solve_span: tracing::Span::none(),
            },
        ),
    );
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .dropped_deregistered
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the deregistered straggler must be dropped"
    );
    assert!(!engine.cycle.results.contains_key(&pid));
    assert_eq!(
        engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed),
        3,
        "the baseline inline-drain merges, and the dropped straggler adds none"
    );
}
/// the detached-merge sidecar thread has NO ambient span
/// context, so every `LaneOutcome::Solved` carrier carries the
/// enqueue-time solve span — the merge-time event (here: the Q1a
/// deregister drop) must land on the carried span rather than
/// orphaning into a Jaeger root. Uses the deregister drop path
/// (unknown pid): deterministic, no registration and no core lock
/// needed. Scoped LOCAL subscriber.
#[cfg(feature = "otel")]
#[test]
fn detached_merge_event_parents_under_the_carried_solve_span() {
    use crate::arb_engine::detached_cycle::detached_merge_sidecar;
    use crate::arb_engine::executor::LaneOutcome;
    use crate::{arb_engine::ArbitrageEngine, otel};
    use alloy::primitives::U256;
    use degenbot_solvers::mixed::SolvePathResult;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
    // tracing::Span binds to the thread-local subscriber at CREATION, so
    // the whole span lifecycle (create → capture → sidecar merge → drop)
    // runs inside one `with_default` scope.
    tracing::subscriber::with_default(subscriber, || {
        let solve_span = tracing::info_span!("degenbot.arb.solve", block.number = 42u64);
        {
            let _guard = solve_span.enter();
            tx.send(LaneOutcome::Solved(
                crate::arb_engine::executor::SolveOutcome {
                    payload: None,
                    worker_clamp_twins: 0,
                    cycle_seq: 1,
                    solve_block: 42,
                    metadata: BlockMetadata::default(),
                    pid: 0xDEAD,
                    update_stamp: Vec::new(),
                    result: SolvePathResult {
                        optimal_input: U256::ZERO,
                        profit: U256::ZERO,
                        hop_outputs: Vec::new(),
                        consumed_inputs: Vec::new(),
                        state_nonces: Vec::new(),
                        solver_pool_states: Vec::new(),
                    },
                    solve_span: tracing::Span::current(),
                },
            ))
            .expect("sidecar rx is alive");
        }
        drop(tx);
        // Inline sidecar run (this thread): the merge enters the item's
        // carried span — exactly what the real std-thread would see.
        detached_merge_sidecar(&engine, rx, None);
        // This outer handle is the LAST reference to the solve span —
        // dropping it closes (exports) the span.
        drop(solve_span);
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let solve_spans: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
        .collect();
    assert_eq!(
        solve_spans.len(),
        1,
        "exactly one solve span; got: {:?}",
        spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
    );
    let merged_event = solve_spans[0]
        .events
        .events
        .iter()
        .any(|e| e.name == "straggler dropped (path deregistered)");
    assert!(
        merged_event,
        "the sidecar merge-time event must parent under the carried solve span; events: {:?}",
        solve_spans[0].events.events
    );
}
/// AQV6EF AC2 (red-first): a panic inside `merge_detached_item` must be
/// CAUGHT — it becomes a typed drain-death record that trips the SAME
/// sticky cordon as a failed send, and the sidecar must return (not
/// vanish silently, stranding every later send with no signal). The
/// dropped Receiver then makes every later send a COUNTED loss.
#[test]
fn a_panicking_merge_becomes_a_typed_record_and_a_sticky_cordon() {
    use crate::arb_engine::detached_cycle::detached_merge_sidecar;
    use crate::arb_engine::executor::LaneOutcome;
    let owner: &'static degenbot_workers::posture::PostureOwner = std::boxed::Box::leak(
        std::boxed::Box::new(degenbot_workers::posture::PostureOwner::new(
            degenbot_workers::posture::PosturePolicy::doc_defaults(),
        )),
    );
    let engine = std::sync::Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    engine
        .lock()
        .cycle
        .set_merge_panic_hook(std::sync::Arc::new(|pid| {
            assert_ne!(pid, 0x5151_5151, "merge boom");
        }));
    let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
    tx.send(LaneOutcome::Suppressed { pid: 0x5151_5151 })
        .expect("rx alive");
    let tx_after = tx.clone();
    drop(tx);
    detached_merge_sidecar(&engine, rx, Some(owner));
    assert_eq!(
        owner.current(),
        degenbot_workers::posture::FleetPosture::Cordoned,
        "a caught merge panic must trip the sticky drain-death cordon"
    );
    assert!(
        tx_after.send(LaneOutcome::Suppressed { pid: 0x1 }).is_err(),
        "the panicked sidecar has exited: later sends hit the dead pipe (the send-failure signal)"
    );
}
/// T2 cadence acceptance, SZJUKL-port: with detached
/// cycles ON through the PRODUCTION stage surface (`EngineStages` — the
/// shipped `solve_dirty` cadence the driver executes INLINE at the machine's
/// decision points), each solve call RETURNS at enqueue-end (µs) while the
/// 3000ms straggler still merges on the sidecar — consecutive stage cycles
/// interleave with the merges, and the dissolved B3 frozen-drainer
/// detector has no queue left to stall across detached cycles (the
/// no-progress obligation lives on the machine's `WatchdogPhase`). The
/// four-call cadence must complete inside the single slow-solve window
/// (enqueue-end, not apply-end), and the stragglers must still land.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_stragglers_do_not_block_inline_stage_work() {
    let (engine, pool_ids, path_ids) = detached_fixture(3000);
    let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
    let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
    for &p in &pool_ids {
        delta.record_affected(HopType::V2, p, 0u64);
    }
    let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
    let meta = BlockMetadata::default();
    // The affected keys the dissolved coordinator would have taken from
    // the epoch ledger before driving the engine's solve.
    let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
        .iter()
        .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
        .collect();
    let t0 = std::time::Instant::now();
    // The detached cycle: the solve call RETURNS (enqueue-end) while the
    // 3000ms slow solve still runs — no in-cycle multi-second hold.
    stages.run_solve_cycle(&affected_keys_v2, 100, &meta);
    // The shipped cadence continues UNCHANGED mid-merge: a debounce publish
    // + further block cycles interleave with the sidecar's merges.
    compute_diff_and_send(&mut engine.lock(), &meta);
    stages.run_solve_cycle(&[], 101, &meta);
    stages.run_solve_cycle(&[], 102, &meta);
    assert!(
            t0.elapsed() < std::time::Duration::from_millis(2500),
            "the whole cadence must complete inside the 3000ms slow-solve window (enqueue-end, not apply-end)"
        );
    // The stragglers DID land via the sidecar (cross-cycle merge),
    // without ever blocking the inline stage work along the way.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while engine.lock().cycle.results.len() < 3 {
        assert!(
            std::time::Instant::now() < deadline,
            "all detached stragglers must merge via the sidecar"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let guard = engine.lock();
    assert!(path_ids.iter().all(|p| guard.cycle.results.contains_key(p)));
}
