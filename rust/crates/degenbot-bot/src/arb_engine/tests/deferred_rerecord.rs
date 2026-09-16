use super::*;

// the deferred-path carry — the ledger re-record at the deferral
// site. The future-price tripwire is unreachable after the solve-anchor
// head floor (only a mid-solve state advance can trip it), so these tests
// install the `force_deferred` seam to exercise the carry
// deterministically. The re-record hook itself is the production seam
// the `EngineStages` constructor installs (`set_deferred_re_record`);
// with the hook unset (direct engine drives) the deferral keeps today's
// drop.
// -------------------------------------------------------------------
fn kjwik5_key(pool_id: u64) -> degenbot_solvers::affected_keys::AffectedKey {
    degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, pool_id)
}
fn kjwik5_affected_keys(pool_ids: &[u64]) -> Vec<degenbot_solvers::affected_keys::AffectedKey> {
    pool_ids.iter().copied().map(kjwik5_key).collect()
}
fn kjwik5_path_keys(
    engine: &ArbitrageEngine,
    path_id: u64,
) -> Vec<degenbot_solvers::affected_keys::AffectedKey> {
    engine
        .path_pools()
        .get(&path_id)
        .expect("registered path")
        .pools
        .iter()
        .map(|r| degenbot_solvers::affected_keys::AffectedKey::new(r.hop_type, r.pool_key))
        .collect()
}
/// Guard 1: capture the `[solve-phase]` events' `paths.deferred_future_price`
/// field. Both reporting sites (the resolve funnel and the detached
/// enqueue) carry the same value; the test asserts every captured value.
type Kjwik5EventFields = std::sync::Arc<parking_lot::Mutex<Vec<Vec<(String, String)>>>>;
type Kjwik5ReRecords = std::sync::Arc<
    parking_lot::Mutex<Vec<(Vec<degenbot_solvers::affected_keys::AffectedKey>, u64)>>,
>;
fn kjwik5_capture_deferred_counter(run: impl FnOnce()) -> Vec<u64> {
    use tracing_subscriber::layer::SubscriberExt;
    struct EventCapture(Kjwik5EventFields);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Saver(Vec<(String, String)>);
            impl tracing::field::Visit for Saver {
                fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                    self.0.push((f.name().to_string(), v.to_string()));
                }
                fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
                    self.0.push((f.name().to_string(), v.to_string()));
                }
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.push((f.name().to_string(), format!("{v:?}")));
                }
            }
            let mut saver = Saver(Vec::new());
            event.record(&mut saver);
            self.0.lock().push(saver.0);
        }
    }
    let capture = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let subscriber =
        tracing_subscriber::registry().with(EventCapture(std::sync::Arc::clone(&capture)));
    tracing::subscriber::with_default(subscriber, run);
    let events = capture.lock().clone();
    events
        .into_iter()
        .filter_map(|fields| {
            fields
                .iter()
                .find(|(name, _)| name == "paths.deferred_future_price")
                .and_then(|(_, value)| {
                    value
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<u64>()
                        .ok()
                })
        })
        .collect()
}
/// Red-first (a): a deferred path re-records ALL its hop pools through the
/// engine's installed hook, with the cycle's solve block — the ledger
/// carry that lets the next draw re-include the path through the same
/// freshness ordering.
#[test]
fn deferred_path_re_records_all_hop_pools_via_the_hook() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[0];
    let expected = kjwik5_path_keys(&engine, deferred);
    assert_eq!(expected.len(), 2, "fixture path has two hops");
    engine
        .cycle
        .set_force_deferred_for_test(HashSet::from([deferred]));
    let seen: Kjwik5ReRecords = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen_hook = std::sync::Arc::clone(&seen);
    set_deferred_re_record(
        &mut engine,
        std::sync::Arc::new(move |keys, block| {
            seen_hook.lock().push((keys.to_vec(), block));
        }),
    );
    run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &kjwik5_affected_keys(&pool_ids),
    );
    let calls = seen.lock().clone();
    assert_eq!(calls.len(), 1, "one re-record call per deferred cycle");
    assert_eq!(
        calls[0].1, 100,
        "the re-record targets the cycle's solve block"
    );
    assert_eq!(
        calls[0].0.as_slice(),
        expected.as_slice(),
        "EVERY hop pool of the deferred path, in path order"
    );
    assert!(
        !engine.cycle.results.contains_key(&deferred),
        "the deferred path is not submitted this cycle (no double-submit)"
    );
    for &sibling in &path_ids[1..] {
        assert!(
            engine.cycle.results.contains_key(&sibling),
            "non-deferred siblings still solve"
        );
    }
}
/// Red-first (d): with the hook unset (direct engine drive, no driver),
/// the deferral keeps today's dropped behavior and counts unchanged.
#[test]
fn deferred_path_without_hook_keeps_today_drop_behavior() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[1];
    engine
        .cycle
        .set_force_deferred_for_test(HashSet::from([deferred]));
    run_test_cycle(
        &mut engine,
        100,
        &BlockMetadata::default(),
        &kjwik5_affected_keys(&pool_ids),
    );
    assert!(
        !engine.cycle.results.contains_key(&deferred),
        "with the hook unset the deferred path keeps today's dropped behavior"
    );
    assert!(engine.cycle.results.contains_key(&path_ids[0]));
    assert!(engine.cycle.results.contains_key(&path_ids[2]));
}
/// Red-first (c): `paths.deferred_future_price` semantics are unchanged —
/// same site, same triggers, same counts (0 with no future hop, N with N).
#[test]
fn future_price_counter_semantics_are_unchanged() {
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let affected = kjwik5_affected_keys(&pool_ids);
    let baseline = kjwik5_capture_deferred_counter(|| {
        run_test_cycle(&mut engine, 100, &BlockMetadata::default(), &affected);
    });
    assert!(
        !baseline.is_empty(),
        "the counter is reported at its [solve-phase] sites"
    );
    assert!(
        baseline.iter().all(|&count| count == 0),
        "no future-priced hop → the counter reads 0; got {baseline:?}"
    );
    engine
        .cycle
        .set_force_deferred_for_test(HashSet::from([path_ids[0], path_ids[2]]));
    let forced = kjwik5_capture_deferred_counter(|| {
        run_test_cycle(&mut engine, 101, &BlockMetadata::default(), &affected);
    });
    assert!(
        forced.iter().all(|&count| count == 2),
        "two deferred paths count at the same site; got {forced:?}"
    );
}
/// Red-first (b): the staged carry. Cycle 1 defers the future-priced path
/// and re-records its pools into the ledger; cycle 2 (the anchor caught
/// up) draws it back and solves it against the retry cycle's block.
#[test]
fn staged_deferred_path_carries_via_the_ledger_and_solves_on_the_retry() {
    use crate::arb_engine::EngineStages;
    use crate::bot_core::stage_handlers::{
        QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
    };
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
    use std::sync::Arc;
    let (mut engine, pool_ids, path_ids) = detached_fixture(0);
    let deferred = path_ids[0];
    let deferred_keys = kjwik5_path_keys(&engine, deferred);
    engine
        .cycle
        .set_force_deferred_for_test(HashSet::from([deferred]));
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
        drawn.0.len(),
        pool_ids.len(),
        "cycle 1 draws every seed key"
    );
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    assert!(
        !engine.lock().cycle.results.contains_key(&deferred),
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
    engine
        .lock()
        .cycle
        .set_force_deferred_for_test(HashSet::new());
    let ctx = BlockContext::new(Epoch::at(101), BlockMetadata::default());
    let drawn = stages
        .on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        })
        .expect("resolve hook is infallible");
    let drawn_keys: HashSet<_> = drawn.0.iter().copied().collect();
    for key in &deferred_keys {
        assert!(
            drawn_keys.contains(key),
            "the retry draw re-includes {key:?}"
        );
    }
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    // Q1a: the retried path's solve block is the RETRY cycle's block.
    // Widened ~15s: the sidecar merge shares solve seats with concurrently
    // running tests, so the old 5s valve could expire under load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !engine.lock().cycle.results.contains_key(&deferred) {
        assert!(
            std::time::Instant::now() < deadline,
            "the retried path must solve and merge via the sidecar"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        engine.lock().cycle.cursor.results_block(),
        101,
        "the retried path solves at the retry cycle's block"
    );
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
    use crate::bot_core::{BlockContext, Epoch, EpochDelta};
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
    engine
        .cycle
        .set_force_deferred_for_test(HashSet::from([deferred]));
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
    stages
        .on_solve(&Solve { ctx, paths: drawn })
        .expect("solve hook is infallible");
    let expired_before = engine
        .lock()
        .cycle
        .detached_cycle
        .leads_expired
        .load(Ordering::Relaxed);
    // The re-record targeted cycle 1's solve block.
    let defer_block = engine.lock().cycle.cursor.results_block();
    // Cycle 2: narrow the window to zero and advance one block — the
    // cutoff prunes the deferred lead's bucket before the draw, so it is
    // never redrawn.
    engine
        .lock()
        .cycle
        .set_force_deferred_for_test(HashSet::new());
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
        !drawn.0.iter().any(|key| deferred_keys.contains(key)),
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
