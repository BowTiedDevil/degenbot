use super::*;

/// A `removed: true` V2 Sync log for a registered pool, when its block is
/// within the reorg journal's depth, drives the pump's reorg branch to
/// restore the pool to its pre-fork state via the `ReorgCoordinator` and
/// record it into the epoch `EpochDelta`, and the pump does NOT shut down
/// (it continues processing).
///
/// This pins the pump-level wiring of ADR-006 slice 7: the coordinator's
/// restore+notify (covered in `reorg_coordinator.rs`) is the downstream
/// observable; what is asserted here is that the *pump* routes a
/// `removed: true` log there, and that an in-depth reorg is non-fatal.
/// Incident 2026-08-20 (WS-silent class): a WS subscription stream that
/// ENDS mid-run must notify the sink (`on_pump_ended` — the production
/// `StageHandlers` impl closes the engine delivery channels there), so
/// the Python block/result streams END and the settlement bot fails
/// loudly instead of idling forever (the silent stall operators saw).
#[tokio::test]
async fn stream_end_notifies_sink_on_pump_ended() {
    let pool_addr = Address::from([0x22u8; 20]);
    let (bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    assert!(!sink.pump_ended(), "no premature pump-ended signal");
    let forward = make_v2_sync_log(pool_addr, U256::from(1_000), U256::from(2_000), 7, false);
    // Stream ends immediately after the log -> Ok(None) arm.
    let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(forward))]).boxed();
    pump.run_test_loop(combined, 5).await;
    assert!(
        sink.pump_ended(),
        "stream end must route to sink.on_pump_ended (closes the Python-facing channels)"
    );
}

// -----------------------------------------------------------------
// late-log admission safety (the no-landmine rule).
//
// A tightened settle/debounce window (50ms → 16ms, task VD62GX) may
// admit logs whose delivery jitter carries them PAST their block's
// quiesce/tombstone edge (the first successor log — ADR-008 D1). Every
// such late log must land in a counted, benign, documented state-
// machine path: dropped un-applied (I4: writers are Streaming-confined),
// counted (degenbot.late_log.admitted + the deduped `late_log` bucket),
// NEVER a shutdown/abort that would masquerade as a structural bug.
//
// The completeness verify at the tombstone/Published edge stays the loud
// safety net for genuinely dropped WS logs — these paths cover only
// POST-tombstone delivery jitter, which the verify cannot see.
// -----------------------------------------------------------------

/// A forward log for a block that is ALREADY tombstoned (delivery jitter
/// past the tombstone edge) must NOT shut the pump down (the retired
/// ADR-008 D3 hard-fault behavior). Target contract, task HJ5HWF:
/// - the pump keeps running and later blocks still process normally;
/// - the late log is dropped WITHOUT applying it (no pool-state mutation
///   outside the Streaming window — I4);
/// - the delivery cutoff stays monotone at the last tombstone (I7);
/// - every tombstone finalize still fires exactly once (I5).
#[tokio::test]
async fn late_forward_after_tombstone_is_benign_late_admit() {
    let pool_addr = Address::from([0x33u8; 20]);
    let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
    let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));

    // Stream: Sync@7 (opens block 7), Sync@8 (tombstones 7, cutoff → 7),
    // then a jittered TAIL for block 7 arriving AFTER the tombstone edge
    // (the late admission), then Sync@9 (tombstones 8, cutoff → 8). End.
    let events = vec![
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool_addr,
            U256::from(3_000),
            U256::from(4_000),
            7,
            false,
        ))),
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool_addr,
            U256::from(5_000),
            U256::from(6_000),
            8,
            false,
        ))),
        // LATE: block 7's tail log, delivered after 7 was tombstoned.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool_addr,
            U256::from(9_999),
            U256::from(8_888),
            7,
            false,
        ))),
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool_addr,
            U256::from(7_000),
            U256::from(8_000),
            9,
            false,
        ))),
    ];
    pump.run_test_loop(stream::iter(events).boxed(), 5).await;
    drainer_settle(|| sink.finalized.lock().unwrap().len() >= 2).await;

    // THE no-landmine assertion: lateness must never look structural.
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "a late forward after the tombstone edge must NOT shut the pump down"
    );
    // Both tombstone finalizes fired exactly once (I5 at the Finalize row).
    let finalized: Vec<u64> = sink
        .finalized
        .lock()
        .unwrap()
        .iter()
        .map(|(b, _)| *b)
        .collect();
    assert_eq!(
        finalized.len(),
        2,
        "both tombstones (7 and 8) must finalize exactly once (got {finalized:?})"
    );
    assert!(
        finalized.contains(&7) && finalized.contains(&8),
        "tombstone finalizes for 7 and 8 must both fire (got {finalized:?})"
    );
    // Delivery cutoff monotone: advanced to the LAST tombstoned block;
    // the late log for 7 must not regress it (I7).
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pump_complete_cutoff(),
        8,
        "cutoff must rest at the last tombstone (8), untouched by the late log"
    );
    // Streaming-only writes: exactly the 3 in-window logs were applied
    // (blocks 7, 8, 9) — the dropped late log never dispatches (I4).
    assert_eq!(
        sink.logs_recorded(),
        3,
        "only the in-Streaming-window logs apply; the late log is dropped"
    );
    // Pool state holds the LAST legitimately applied sync (9's), never
    // the late log's reserves — either applying the late log after 9's
    // or counting it into the applies would trip this.
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .v2_snapshot(pool_id),
        Some((U256::from(7_000), U256::from(8_000), 9)),
        "pool state reflects only Streaming-window applies; late reserves dropped"
    );
}

// HJ5HWF property: synthetic lateness across a whole capture. The tail
// of EVERY block's log set (a randomized subset) is jitted past its
// quiesce/tombstone edge (delivered behind the successor's first log).
// Invariants asserted per run:
// - no tripwire/fatal path fires from lateness (pump never shuts down);
// - the delivery cutoff stays monotone at the last tombstone (I7);
// - no pool-state mutation outside Streaming: apply accounting counts
//   exactly the in-window deliveries — late tails are benign drops (I4);
// - one tombstone finalize per block, no superfluous publishes (I5);
// - the reorg recovery-drop class is NOT a bucket for lateness: no
//   recovery anchor is ever established by a jitter stream, so no
//   late log may be mis-filed there (it takes the late-admit path).
proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(16))]
    #[test]
    fn synthetic_lateness_jitter_never_trips_fatal_paths(
        plan in proptest::collection::vec((1u8..=3, 0u8..=2), 2usize..=6),
    ) {
        use stream::StreamExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        // The async case reports its first invariant breach as an Err
        // message; the `return Err` below targets the proptest run
        // closure (a property failure must surface as a TestCaseError,
        // and a `return` inside an async block cannot reach it).
        let check = async {
            let pool_addr = Address::from([0x34u8; 20]);
            let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
            let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));

            let base = 100u64;
            // Distinct reserves per (block, log-index) so any mis-applied
            // (late) log is observable in the final state.
            let reserves = |block: u64, j: usize| {
                let k = 1_000u64 + block * 10 + j as u64;
                (U256::from(k), U256::from(2_000 + k))
            };
            let log_event = |block: u64, j: usize| {
                let (r0, r1) = reserves(block, j);
                WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                    pool_addr, r0, r1, block, false,
                )))
            };

            // Weave the stream: per block, deliver pre-tail logs; every
            // NON-LAST block's tail is deferred until just after the next
            // block's FIRST log (the tombstone/quiesce edge) — the
            // synthetic-jitter shape for a tightened settle window. The
            // last block's tail is still inside its open Streaming window
            // and applies normally.
            let mut events: Vec<WsEvent> = Vec::new();
            let mut deferred_tail: Vec<WsEvent> = Vec::new();
            let mut expected_applied = 0usize;
            let mut expected_finalized: Vec<u64> = Vec::new();
            let (mut last_reserves, mut last_block) = ((U256::ZERO, U256::ZERO), 0u64);
            for (i, (pre, tail)) in plan.iter().enumerate() {
                let block = base + i as u64;
                // First log of the block = the tombstone edge; the
                // previous block's jittered tail lands right behind it.
                events.push(log_event(block, 0));
                events.append(&mut deferred_tail);
                for j in 1..*pre as usize {
                    events.push(log_event(block, j));
                }
                expected_applied += *pre as usize;
                if i + 1 < plan.len() {
                    // This block's tail jitters past its tombstone edge.
                    deferred_tail = (0..*tail as usize)
                        .map(|j| log_event(block, *pre as usize + j))
                        .collect();
                    expected_finalized.push(block);
                } else {
                    // Last block: tail inside the open window.
                    for j in 0..*tail as usize {
                        events.push(log_event(block, *pre as usize + j));
                    }
                    expected_applied += *tail as usize;
                    let jj = if *tail > 0 {
                        *pre as usize + *tail as usize - 1
                    } else {
                        *pre as usize - 1
                    };
                    last_reserves = reserves(block, jj);
                    last_block = block;
                }
            }

            pump.run_test_loop(stream::iter(events).boxed(), 5).await;
            drainer_settle(|| sink.finalized.lock().unwrap().len() >= expected_finalized.len())
                .await;

            // No-landmine: lateness never trips a fatal path.
            if shutdown.load(Ordering::Relaxed) {
                return Err("synthetic lateness must never shut the pump down".to_owned());
            }
            // I5: exactly one finalize per tombstoned block, none else.
            let finalized: Vec<u64> = sink
                .finalized
                .lock()
                .unwrap()
                .iter()
                .map(|(b, _)| *b)
                .collect();
            if finalized != expected_finalized {
                return Err(format!(
                    "one finalize per block, in order: got {finalized:?}, want {expected_finalized:?}"
                ));
            }
            // I7: cutoff monotone at the last tombstone.
            let cutoff = bot.state_arc().read_at(crate::bot_core::state_lock::LockSite::Pump).pump_complete_cutoff();
            if cutoff != base + plan.len() as u64 - 2 {
                return Err(format!(
                    "delivery cutoff must rest at the last tombstone: got {cutoff}"
                ));
            }
            // I4: applies == in-window deliveries exactly.
            let recorded = sink.logs_recorded();
            if recorded != expected_applied {
                return Err(format!(
                    "only in-window logs may apply (I4): got {recorded}, want {expected_applied}"
                ));
            }
            // The last LEGITIMATELY applied log owns pool state.
            let snap = bot.state_arc().read_at(crate::bot_core::state_lock::LockSite::Pump).v2_snapshot(pool_id);
            if snap != Some((last_reserves.0, last_reserves.1, last_block)) {
                return Err(format!(
                    "pool state holds the last in-window apply, not a late tail: got {snap:?}"
                ));
            }
            Ok(())
        };
        if let Err(msg) = rt.block_on(check) {
            return Err(proptest::test_runner::TestCaseError::fail(msg));
        }
    }
}

#[tokio::test]
async fn reorg_log_restores_pool_via_coordinator_and_pump_continues() {
    let pool_addr = Address::from([0x11u8; 20]);
    let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);

    // Forward Sync at block 7 — misprices the pool and seeds the journal
    // genesis(5) → transition(7). Drive through the *pump* (not
    // `bot.dispatch_log` directly) so the same code path that handles
    // live WS logs is exercised.
    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    let forward = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
    // Stream ends immediately after the log; the loop returns via the
    // `Ok(None)` arm (both subscription streams ended) once the reorg
    // branch `continue`s and the stream is exhausted.
    let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(forward))]).boxed();
    pump.run_test_loop(combined, 5).await;

    assert!(
        bot.active_delta()
            .snapshot_keys()
            .contains(&AffectedKey::new(HopType::V2, pool_id)),
        "forward Sync through the pump recorded the pool into the EpochDelta"
    );
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .v2_snapshot(pool_id),
        Some((U256::from(1_500), U256::from(2_500), 7)),
        "forward Sync applied through the pump",
    );
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "no reorg yet — pump running"
    );

    // Reorg: a removed-flag Sync at block 7 rolls back to genesis. Build a
    // fresh pump over the SAME bot (the journal + state persist on `Bot`)
    // and feed only the removed log.
    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    let reorg_log = make_v2_sync_log(
        pool_addr,
        U256::from(1_500), // content unused — block + pool identity matter
        U256::from(2_500),
        7,
        true,
    );
    let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(reorg_log))]).boxed();
    pump.run_test_loop(combined, 5).await;

    assert!(
        bot.active_delta()
            .snapshot_keys()
            .contains(&AffectedKey::new(HopType::V2, pool_id)),
        "reorg re-recorded the restored pool into the EpochDelta"
    );
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .v2_snapshot(pool_id),
        Some((U256::from(1_000), U256::from(2_000), 5)),
        "reorg rolled back to genesis reserves",
    );
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "in-journal-depth reorg is non-fatal — pump did NOT shut down"
    );
}

/// A too-deep reorg (the removed log's block is at/below the journal's
/// earliest delta) returns `Err(NoStatePriorToBlock)` from the
/// coordinator; the pump treats this as unrecoverable — it sets the
/// shutdown flag and returns from `run_with_stream` so Python observes
/// the pump task exiting, rather than continuing with stale state.
#[tokio::test]
async fn too_deep_reorg_shuts_down_pump_gracefully() {
    let pool_addr = Address::from([0x22u8; 20]);
    // Genesis anchored at block 5 — restore_before_block(5) is too deep
    // (nothing the journal can land on prior to the genesis delta).
    let (bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    // Removed-flag Sync at block 5 → coordinator restores before 5, which
    // is at the journal's genesis floor → `Err(NoStatePriorToBlock)`.
    let reorg_log = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 5, true);
    let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(reorg_log))]).boxed();
    pump.run_test_loop(combined, 5).await;

    assert!(
        shutdown.load(Ordering::Relaxed),
        "too-deep reorg must set the shutdown flag",
    );
    // `run_test_loop` returned (this assert is reached), proving the pump
    // exited its loop instead of looping forever on a fatal reorg.
}

/// ADR-008 D3 pump-level: a contiguous `removed: true` chunk (delivered
/// in REVERSE log-index order — nodes may emit reorg events unordered)
/// enters + continues the reorg path, restoring the pool per-event via the
/// coordinator; the first `removed: false` event after entry closes the
/// window, its block becomes the new head, and the pump CONTINUES (no
/// shutdown). The forward log at the new head re-applies against the
/// restored state.
#[tokio::test]
async fn reorg_contiguous_chunk_closes_on_first_forward_and_continues() {
    let pool_addr = Address::from([0x33u8; 20]);
    // Genesis anchored at block 5: reserves (1000, 2000).
    let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
    let snapshot = || {
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .v2_snapshot(pool_id)
    };

    // Drive 5 -> 7 (forward sync at 7) -> tombstone 7 via a forward sync
    // at 8 (advance_to_drained(7) follows the tombstone).
    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
    let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(s7)),
        WsEvent::Pool(PoolEvent::from_log(s8)),
    ])
    .boxed();
    pump.run_test_loop(combined, 5).await;
    assert!(
        bot.active_delta()
            .snapshot_keys()
            .contains(&AffectedKey::new(HopType::V2, pool_id)),
        "forward syncs recorded the pool into the EpochDelta"
    );
    assert_eq!(snapshot(), Some((U256::from(1_600), U256::from(2_600), 8)));
    assert!(!shutdown.load(Ordering::Relaxed));

    // Reorg over blocks 7 and 8: removed logs arrive in REVERSE order
    // (8 then 7), then the first removed:false at block 9 closes it.
    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    let r8 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 8, true);
    let r7 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 7, true);
    let s9 = make_v2_sync_log(pool_addr, U256::from(1_700), U256::from(2_700), 9, false);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(r8)),
        WsEvent::Pool(PoolEvent::from_log(r7)),
        WsEvent::Pool(PoolEvent::from_log(s9)),
    ])
    .boxed();
    pump.run_test_loop(combined, 5).await;

    // The reorg unwound 7 and 8 (restore to genesis), then the forward
    // sync at 9 re-applied -> reserves reflect block 9's values.
    assert_eq!(snapshot(), Some((U256::from(1_700), U256::from(2_700), 9)));
    assert!(
        !shutdown.load(Ordering::Relaxed),
        "reorg path closed cleanly — pump did NOT shut down"
    );
}

/// HJ5HWF pump-level (supersedes the retired ADR-008 D3 hard-fault
/// behavior, the no-landmine ruling): a `removed: false` log on a
/// tombstoned block (NOT a reorg) is delivery-jitter LATENESS. The pump
/// takes the benign late-admit path: the late log is dropped UN-applied,
/// counted (`degenbot.late_log.admitted` + the deduped `late_log`
/// bucket, mapped in the trace as `LateAdmitDropped`), and the pump
/// KEEPS RUNNING. No silent re-apply: the cutoff never regresses and the
/// pool's state pins are untouched by the late survivor.
#[tokio::test]
async fn late_forward_log_on_tombstoned_block_is_benign_late_admit() {
    let pool_addr = Address::from([0x44u8; 20]);
    let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);

    // Single pump session: forward sync(7) opens block 7; forward sync(8)
    // tombstones 7 (open block becomes 8); THEN a forward (removed:false)
    // sync at block 7 arrives late — block 7 is tombstoned and the open
    // block is 8 -> late forward -> benign late-admit drop.
    let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
    let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
    let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
    let late = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 7, false);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(s7)),
        WsEvent::Pool(PoolEvent::from_log(s8)),
        WsEvent::Pool(PoolEvent::from_log(late)),
    ])
    .boxed();
    pump.run_test_loop(combined, 5).await;
    drainer_settle(|| sink.logs_recorded() >= 2).await;

    assert!(
        !shutdown.load(Ordering::Relaxed),
        "late removed:false on a tombstoned block is counted lateness — the pump must keep running"
    );
    // Exactly the two in-window logs applied; the late survivor dropped.
    assert_eq!(
        sink.logs_recorded(),
        2,
        "the late forward must never dispatch (I4: Streaming-only writes)"
    );
    // Cutoff rests at the last tombstone — the late log for 7 moved
    // neither the cutoff nor the pool state (I7).
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pump_complete_cutoff(),
        7,
        "cutoff rests at the tombstoned block 7"
    );
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .v2_snapshot(pool_id),
        Some((U256::from(1_600), U256::from(2_600), 8)),
        "pool state holds the last in-window apply, never the late survivor"
    );
}

/// FSM RECOVERY green path: after the header-staleness watchdog
/// performs an authoritative catch-up to block 102 (`recovery_anchor = 102`),
/// a recovering WS flushes a buffered forward Sync log at block 102 (≤ the
/// anchor). It is a single-writer duplicate of the already-applied backfill
/// and MUST be dropped — NOT a false ADR-008 D3 shutdown.
///
/// This is the exact observed failure (block 25670138): catch-up OWNs the
/// range, the delayed WS re-delivers it, and the pump must discard.
#[tokio::test]
async fn recovery_single_writer_discards_stale_forward_after_backfill() {
    let pool_addr = Address::from([0x44u8; 20]);
    let (_bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

    let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_header_staleness_for_test(Duration::from_millis(100));

    // Watchdog path: `get_block_number` → 102 (triggers backfill), then
    // `get_logs(102)` → [] (recovery_anchor = 102). Extra `0x66` pads later
    // ticks (current already 102 → latest>current false → no second backfill).
    asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
    asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());

    // Header 101 (anchor), then silence so the watchdog backfills to 102,
    // then the recovering WS flushes a STALE forward sync at block 102.
    let stale = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 102, false);
    let combined = stream::unfold(0u8, move |phase| {
        let stale = stale.clone();
        async move {
            match phase {
                0 => Some((
                    WsEvent::BlockHeader {
                        number: 101,
                        timestamp: 1,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                    },
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((WsEvent::Pool(PoolEvent::from_log(stale)), 2))
                }
                _ => None,
            }
        }
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    assert!(
        !shutdown.load(Ordering::Relaxed),
        "a stale forward ≤ recovery_anchor (single-writer duplicate) must be discarded, not fatal"
    );
}

/// BQ7ZBC × HJ5HWF — FSM guard: the single-writer discard is scoped to
/// blocks the pump itself backfilled (≤ `recovery_anchor`). The
/// header-staleness watchdog catch-up anchors at 102, then a
/// `removed:false` forward at block 103 arrives late (103 tombstoned by
/// 104, 103 > 102) — OUTSIDE the silent single-writer duplicate class, so
/// it lands in the BENIGN late-admit drop: dropped un-applied, counted in
/// `degenbot.late_log.admitted`, pump keeps running (the no-landmine
/// ruling supersedes the retired ADR-008 D3 hard fault).
#[tokio::test]
async fn recovery_anchor_stale_forward_above_anchor_is_benign_late_admit() {
    let pool_addr = Address::from([0x44u8; 20]);
    let (_bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

    let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_header_staleness_for_test(Duration::from_millis(100));

    asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
    asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());

    let s103 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 103, false);
    let s104 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 104, false);
    let late103 = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 103, false);
    let combined = stream::unfold(0u8, move |phase| {
        let s103 = s103.clone();
        let s104 = s104.clone();
        let late103 = late103.clone();
        async move {
            match phase {
                0 => Some((
                    WsEvent::BlockHeader {
                        number: 101,
                        timestamp: 1,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                    },
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((WsEvent::Pool(PoolEvent::from_log(s103)), 2))
                }
                2 => Some((WsEvent::Pool(PoolEvent::from_log(s104)), 3)),
                3 => Some((WsEvent::Pool(PoolEvent::from_log(late103)), 4)),
                _ => None,
            }
        }
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    assert!(
            !shutdown.load(Ordering::Relaxed),
            "a stale forward ABOVE recovery_anchor is counted lateness — the pump keeps running (HJ5HWF no-landmine)"
        );
}

/// FULL FSM lifecycle on a mocked websocket. One session drives
/// `LIVE → RESET/CATCH_UP → back-to-LIVE`:
///   1. LIVE: a forward Sync@102 is applied (reserves 1500/2500).
///   2. Stall → the header-staleness watchdog does an authoritative catch-up
///      to block 103 (mocked `eth_blockNumber`/`eth_getLogs`), setting
///      `recovery_anchor = 103` (the RESET transition).
///   3. back-to-LIVE: a fresh Sync@104 (> anchor) is applied (reserves
///      2600/3600).
///   4. A recovering WS then flushes a STALE Sync@103 (9999/9999, ≤ anchor).
///      The single-writer discard must DROP it — if it were re-asserted it
///      would overwrite the pool reserves back to the older 9999/9999.
/// Asserts: the pump did NOT shut down (it survived the recovery) AND the
/// V2 pool reserves are 2600/3600 (the stale log was not re-applied).
#[tokio::test]
async fn fsm_lifecycle_recovers_and_does_not_reassert_stale() {
    let pool_addr = Address::from([0x44u8; 20]);

    let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_header_staleness_for_test(Duration::from_millis(100));
    // Register the pool on the pump's OWN bot (the one it applies logs to),
    // so the V2 reserves reflect the dispatched Sync events.
    let bot = pump.bot_arc_for_test();
    {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.register_v2_pool(&RegisterV2PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            reserve0: U112::from(1_000),
            reserve1: U112::from(2_000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xf0u8; 20]),
            update_block: 100,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    }

    // Watchdog catch-up to 103: `get_block_number` → 103, then `get_logs`
    // for the range → []. Extra `0x67` pads later ticks (once caught up,
    // `latest > current` is false → no second backfill).
    asserter.push_success(&"0x67".to_string()); // eth_blockNumber → 103
    asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(range) → []
    asserter.push_success(&"0x67".to_string());
    asserter.push_success(&"0x67".to_string());
    asserter.push_success(&"0x67".to_string());

    let s102 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 102, false);
    let s104 = make_v2_sync_log(pool_addr, U256::from(2_600), U256::from(3_600), 104, false);
    let stale103 = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 103, false);
    let combined = stream::unfold(0u8, move |phase| {
        let s102 = s102.clone();
        let s104 = s104.clone();
        let stale103 = stale103.clone();
        async move {
            match phase {
                0 => Some((
                    WsEvent::BlockHeader {
                        number: 101,
                        timestamp: 1,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                    },
                    1,
                )),
                1 => Some((WsEvent::Pool(PoolEvent::from_log(s102)), 2)),
                2 => {
                    // Stall: let the watchdog catch up, then the WS resumes.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((WsEvent::Pool(PoolEvent::from_log(s104)), 3))
                }
                3 => Some((WsEvent::Pool(PoolEvent::from_log(stale103)), 4)),
                _ => None,
            }
        }
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    assert!(
        !shutdown.load(Ordering::Relaxed),
        "the FSM must survive a stall-recovery and stay alive"
    );
    // The stale Sync@103 must NOT have been re-asserted: final reserves are
    // those of the last applied forward (Sync@104), not the stale 9999/9999.
    let state = bot.state_arc();
    let core = state.read_at(crate::bot_core::state_lock::LockSite::Pump);
    let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
    if let Some(crate::bot_core::PoolEntry::V2(p)) = core.pools.get(&pool_id) {
        let pool = &p.1;
        assert_eq!(
            pool.reserve0.to::<u128>(),
            2_600,
            "stale forward ≤ recovery_anchor must be dropped, not re-asserted"
        );
        assert_eq!(
            pool.reserve1.to::<u128>(),
            3_600,
            "stale forward ≤ recovery_anchor must be dropped, not re-asserted"
        );
    } else {
        panic!("test setup: V2 pool not found for {pool_addr}");
    }
}
