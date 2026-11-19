//! The pump loop's independent liveness clock.
//!
//! The in-loop `staleness_tick` is driven by the loop's own `select!`, so it can
//! only fire while the loop is parked AT that select. A loop parked BEFORE the
//! select — a `PySubscriberAdapter` GIL re-entry inside `drive_publish`, engine
//! lock contention inside the stage hooks — polls none of its own timers, so its
//! silence is invisible to every clock it owns. These scenarios pin the
//! independent clock: arrivals are beaten on the pump's task, and the stall
//! window is observed on a SEPARATE task that keeps running while the loop does
//! not.

use super::*;
use crate::arb_engine::SessionEndCause;

/// The stall window these scenarios drive: far longer than the loop's own 500ms
/// cooperative-tick arrival (so a healthy parked-at-select loop never trips it)
/// and short enough to be an unremarkable span of virtual time.
const STALL: Duration = Duration::from_millis(200);

/// The block-header shape every scenario reuses.
fn header(number: u64) -> WsEvent {
    WsEvent::BlockHeader {
        number,
        timestamp: number,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
        hash: alloy::primitives::B256::ZERO,
        parent_hash: alloy::primitives::B256::ZERO,
    }
}

/// Spawn the pump loop on its own task. These scenarios need the loop to keep
/// running while the TEST observes the clock — the whole point is that a parked
/// loop cannot be its own observer.
fn spawn_pump(
    mut pump: BlockPump,
    combined: stream::BoxStream<'static, WsEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { pump.run_test_loop(combined, 100).await })
}

/// A GIL re-entry park: the pump task cannot reach its select, so none of its
/// own timers advance. The independent clock still observes the silence, and the
/// close it requests lands through the loop's OWN machinery — never an abort.
#[tokio::test(start_paused = true)]
async fn a_gil_reentry_park_is_a_stall_the_clock_closes_without_an_abort() {
    let (mut pump, sink, _asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_stall_window_for_test(STALL);
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    pump.set_park_gate_for_test(Arc::clone(&gate));
    let facts = pump.stall_facts();

    // The GIL, held by the Python side: the pump's re-entry waits on it.
    let gil = gate.lock().await;

    let started = tokio::time::Instant::now();
    let mut pump_task = spawn_pump(pump, stream::pending().boxed());
    // Let the loop take its first arrival and park on the held gate.
    tokio::task::yield_now().await;

    let cause = tokio::time::timeout(STALL * 3, facts.wait()).await.expect(
        "the independent clock must publish the stall fact while the pump loop is \
         parked before its select",
    );
    assert_eq!(cause, SessionEndCause::StallWatchdogTripped);
    assert!(
        started.elapsed() <= STALL * 3,
        "the stall must be detected inside a small multiple of the window, took {:?}",
        started.elapsed()
    );
    assert!(
        shutdown.load(Ordering::Relaxed),
        "the stall must request the cooperative close"
    );

    // Release the GIL: the loop reaches its own control point and unwinds.
    drop(gil);
    tokio::time::timeout(Duration::from_secs(1), &mut pump_task)
        .await
        .expect("the released loop must unwind through its own close path")
        .expect("the pump loop task must not have panicked");
    assert!(
        sink.pump_ended(),
        "the loop's ONE exit edge must run for the stall close"
    );
}

/// Engine-lock contention arriving mid-flight: arrivals flow (the clock is
/// healthy and must stay quiet), then a lock holder parks the loop, and the
/// silence that follows is what the clock reports.
#[tokio::test(start_paused = true)]
async fn lock_contention_after_healthy_arrivals_is_a_stall_the_clock_reports() {
    let (mut pump, sink, _asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_stall_window_for_test(STALL);
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    pump.set_park_gate_for_test(Arc::clone(&gate));
    let facts = pump.stall_facts();

    // Six headers 40ms apart: arrivals well inside the window.
    let combined = stream::unfold(0u64, |n| async move {
        if n >= 6 {
            // The WS stays silent for the rest of the scenario — the stream
            // never ends, so only a close can stop the loop.
            std::future::pending::<()>().await;
            return None;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        Some((header(101 + n), n + 1))
    })
    .boxed();

    let mut pump_task = spawn_pump(pump, combined);
    tokio::time::sleep(Duration::from_millis(240)).await;
    assert_eq!(
        facts.current(),
        None,
        "a flowing header stream must never publish a stall fact"
    );
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "a flowing header stream must never request a close"
    );

    // The engine lock, held elsewhere: the loop's next arrival parks on it.
    let lock = gate.lock().await;
    let cause = tokio::time::timeout(STALL * 6, facts.wait())
        .await
        .expect("lock contention that parks the loop must be reported by the independent clock");
    assert_eq!(cause, SessionEndCause::StallWatchdogTripped);
    assert!(shutdown.load(Ordering::Relaxed));

    drop(lock);
    tokio::time::timeout(Duration::from_secs(1), &mut pump_task)
        .await
        .expect("the released loop must unwind through its own close path")
        .expect("the pump loop task must not have panicked");
    assert!(sink.pump_ended());
}

/// Dense logs ARE arrivals: a burst that never stops beating must not trip the
/// clock however many windows it spans.
#[tokio::test(start_paused = true)]
async fn dense_log_flow_is_never_a_stall() {
    let (mut pump, _sink, _asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_stall_window_for_test(STALL);
    let facts = pump.stall_facts();

    // Header + V2 Sync pairs every 10ms for 40 blocks: 800ms of arrivals, four
    // stall windows long.
    let pool = Address::from([0x11u8; 20]);
    let combined = stream::unfold((101u64, 0u8, pool), |(block, toggle, pool)| async move {
        if block > 140 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let event = if toggle == 0 {
            header(block)
        } else {
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::ZERO,
                U256::ZERO,
                block,
                false,
            )))
        };
        Some((event, (block + u64::from(toggle), toggle ^ 1, pool)))
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    assert_eq!(
        facts.current(),
        None,
        "a dense log flow must never publish a stall fact"
    );
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "a dense log flow must never request a close"
    );
}
