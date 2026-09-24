use super::*;

/// Solve-anchor regression (ADR-008 D2 solver-release gate): the SOLVE anchor
/// follows the LOG-DRIVEN settled block (`open`), not a header that raced a
/// RED→GREEN tracer : the pump forwards a
/// `BlockNotification` for every `newHeads` header it accepts (one per
/// header, carrying the header's number + metadata), via
/// `StageHandlers::notify_block` — independent of solve/debounce state. This
/// is the seam that lets Python derive its block clock from `newHeads`
/// instead of the stale `ResultBatch::solve_block`.
#[tokio::test]
async fn notify_block_fires_once_per_accepted_header() {
    let (mut pump, sink) = pump_for_test(Some(100));
    let meta_101 = BlockMetadata {
        timestamp: 1_700_000_100,
        base_fee_per_gas: Some(1_000_000_001),
        gas_used: 10_000_001,
        gas_limit: 30_000_001,
    };
    let meta_102 = BlockMetadata {
        timestamp: 1_700_000_200,
        base_fee_per_gas: Some(2_000_000_002),
        gas_used: 20_000_002,
        gas_limit: 30_000_002,
    };
    let events: Vec<WsEvent> = vec![
        WsEvent::BlockHeader {
            number: 101,
            timestamp: meta_101.timestamp,
            base_fee_per_gas: meta_101.base_fee_per_gas,
            gas_used: meta_101.gas_used,
            gas_limit: meta_101.gas_limit,
        },
        WsEvent::BlockHeader {
            number: 102,
            timestamp: meta_102.timestamp,
            base_fee_per_gas: meta_102.base_fee_per_gas,
            gas_used: meta_102.gas_used,
            gas_limit: meta_102.gas_limit,
        },
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;

    let notified = sink.notified.lock().unwrap().clone();
    assert_eq!(
        notified.len(),
        2,
        "exactly one notify_block per accepted header"
    );
    assert_eq!(notified[0].0, 101);
    assert_eq!(notified[0].1, meta_101);
    assert_eq!(notified[1].0, 102);
    assert_eq!(notified[1].1, meta_102);
}

/// 5DM6JJ contract: the cold-start branch in `run_with_stream` must honor
/// `first_observed_block` when `last_processed_block()` is `None` (no
/// prior anchor). This is the defensive safety net the legacy `spawn` fix
/// leans on: passing the REAL subscribe block W (instead of the legacy
/// hard-coded `0`) means that if the `on_drain(first_block)` anchor were
/// ever absent, the pump still cold-starts to W — NOT stuck at 0 to be
/// jumped out-of-order by the first WS log. Under ADR-008 the tombstone is
/// a real log for W+1 (not a header).
#[tokio::test]
async fn cold_start_anchors_to_first_observed_block() {
    // No prior processed block → `current_block` starts at 0. Pass the
    // subscribe block W as `first_observed_block`. The cold-start branch
    // anchors `current_block` to W. header(W) is the first header
    // (anchor, no finalize); a forward log for W+1 tombstones W →
    // finalize(W) carrying meta_w. Proves we cold-started to W, not 0.
    let (mut pump, sink) = pump_for_test(None);
    let w = 21_500_000u64; // a "huge" chain-head block number
    let meta_w = BlockMetadata {
        timestamp: 1,
        base_fee_per_gas: Some(7),
        gas_used: 8,
        gas_limit: 9,
    };
    let meta_w1 = BlockMetadata {
        timestamp: 2,
        base_fee_per_gas: Some(10),
        gas_used: 11,
        gas_limit: 12,
    };
    let tombstone_log = make_v2_sync_log(
        Address::from([0xfcu8; 20]),
        U256::from(1),
        U256::from(2),
        w + 1,
        false,
    );
    let events: Vec<WsEvent> = vec![
        WsEvent::BlockHeader {
            number: w,
            timestamp: meta_w.timestamp,
            base_fee_per_gas: meta_w.base_fee_per_gas,
            gas_used: meta_w.gas_used,
            gas_limit: meta_w.gas_limit,
        },
        WsEvent::BlockHeader {
            number: w + 1,
            timestamp: meta_w1.timestamp,
            base_fee_per_gas: meta_w1.base_fee_per_gas,
            gas_used: meta_w1.gas_used,
            gas_limit: meta_w1.gas_limit,
        },
        WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, w).await;
    drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

    let finalized = sink.finalized.lock().unwrap().clone();
    assert!(!finalized.is_empty(), "log w+1 should tombstone+finalize w");
    assert_eq!(
        finalized[0].0, w,
        "first finalize is for the anchored block w"
    );
    assert_eq!(
        finalized[0].1, meta_w,
        "block w's batch carries w's metadata (in-order, anchored)"
    );
}

/// BGEDB6 (3M5PO5 correction): the delivery cutoff (last complete block)
/// is owned by `BotState` and outlives a pump run. A second
/// `run_with_stream` (a resume with a fresh `StageMachine`)
/// must NOT reset it — the old design re-embedded a fresh
/// `Arc<AtomicU64>` (starting at 0) into `BotState` at startup, and the
/// registration drain stalled until every block re-tombstoned.
#[tokio::test]
async fn resume_never_resets_pump_complete_cutoff() {
    use stream::StreamExt;
    let header = |n: u64| WsEvent::BlockHeader {
        number: n,
        timestamp: n,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    };
    let bot = Arc::new(Bot::new(1));
    let w = 21_500_000u64;

    // Run 1: header(w) + header(w+1) + a forward log for w+1 -> tombstone w.
    let (mut pump1, _sink1, _shutdown1) = pump_for_test_with_bot(Arc::clone(&bot), None);
    let events: Vec<WsEvent> = vec![
        header(w),
        header(w + 1),
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            w + 1,
            false,
        ))),
    ];
    pump1.run_test_loop(stream::iter(events).boxed(), w).await;
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pump_complete_cutoff(),
        w,
        "run 1's tombstone of w must reach the state-owned cutoff"
    );

    // Run 2 (resume): a fresh pump, fresh FSM + clock. One header, no new
    // logs -> no new tombstone. The cutoff must survive, not reset.
    let (mut pump2, _sink2, _shutdown2) = pump_for_test_with_bot(Arc::clone(&bot), None);
    pump2
        .run_test_loop(stream::iter(vec![header(w + 1)]).boxed(), w)
        .await;
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pump_complete_cutoff(),
        w,
        "a resume must NOT reset the cutoff — the value outlives the run"
    );
}

// ==============================================================
// T3: the single-writer boundary rule has one owner —
// the FSM's recovery anchor + `should_drop_recovered_forward` (the
// BQ7ZBC drop path). The driver seeds the anchor from the resume
// boundary; no inline `snapshot_seed` check remains in the log loop.
// ==============================================================

/// DFQYM5 single-writer regression for the resume boundary: with the
/// snapshot→WS gap backfilled (S < W), the WS's partial duplicate of W
/// (the boundary block the backfill already fully applied) must not be
/// re-applied, while the first LIVE log (W+1) flows through. Pins the
/// behavior T3 preserves while the drop rule's owner moves from the
/// inline driver check to the FSM's recovery anchor.
#[tokio::test]
async fn resume_boundary_duplicate_dropped_live_block_applied() {
    use stream::StreamExt;

    let bot = Arc::new(Bot::new(1));
    let w = 21_500_000u64;
    let pool = Address::from([0xc0u8; 20]);
    let pool_id = {
        let arc = bot.state_arc();
        let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
        let pool_id = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: pool,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                update_block: w - 10,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        // Simulate the backfill having applied W (the boundary): state +
        // the drain cutoff both land at W.
        let _ = core.apply_sync_by_pool_id(pool_id, U112::from(5_000), U112::from(1_000), w);
        core.advance_pump_complete_cutoff(w);
        core.set_snapshot_seed_block(Some(w - 10)); // S < W -> backfill owned
        pool_id
    };

    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);
    sink.set_dirty(true);

    let header = |n: u64| WsEvent::BlockHeader {
        number: n,
        timestamp: n,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    };
    let dup_w = make_v2_sync_log(pool, U256::from(5_500u64), U256::from(900u64), w, false);
    let live_w1 = make_v2_sync_log(pool, U256::from(6_000u64), U256::from(950u64), w + 1, false);
    pump.run_test_loop(
        stream::iter(vec![
            header(w + 1),
            WsEvent::Pool(PoolEvent::from_log(dup_w)),
            WsEvent::Pool(PoolEvent::from_log(live_w1)),
        ])
        .boxed(),
        w,
    )
    .await;

    let arc = bot.state_arc();
    let core = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
    let st = core.get_v2_pool_state(pool_id).expect("v2 state");
    assert_eq!(
        st.reserve0,
        U112::from(6_000),
        "W's partial duplicate must NOT re-apply (backfill owns [S+1, W])"
    );
    assert_eq!(st.update_block, w + 1, "the live W+1 log applies");
}

/// The T3 behavior delta: a `removed: true` (reorg) log at or below the
/// resume boundary must REACH the reorg classifier — the single-writer
/// drop rule only exempts forward logs. Before T3 the inline
/// `snapshot_seed` check silently dropped reorg logs at the boundary
/// (a deep-reorg re-delivery could never unwind the backfilled range);
/// after T3 `should_drop_recovered_forward(removed: true)` is false and
/// `ReorgCoordinator` restores the pool's pre-block state.
///
/// (RED on pre-T3 code: the reorg log drops inline and the pool stays at
/// the backfilled-at-W reserves.)
#[tokio::test]
async fn resume_boundary_reorg_reaches_classifier_not_inline_drop() {
    use stream::StreamExt;

    let bot = Arc::new(Bot::new(1));
    let w = 21_500_000u64;
    let pool = Address::from([0xc1u8; 20]);
    let pool_id = {
        let arc = bot.state_arc();
        let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
        let pool_id = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: pool,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                update_block: w - 10,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        // Backfill-applied state: w-5 then W (both inside [S+1, W]).
        let _ = core.apply_sync_by_pool_id(pool_id, U112::from(3_000), U112::from(1_500), w - 5);
        let _ = core.apply_sync_by_pool_id(pool_id, U112::from(5_000), U112::from(1_000), w);
        core.advance_pump_complete_cutoff(w);
        core.set_snapshot_seed_block(Some(w - 10)); // S < W -> backfill owned
        pool_id
    };

    let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);

    let header = |n: u64| WsEvent::BlockHeader {
        number: n,
        timestamp: n,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    };
    // The WS re-delivers the removed boundary Sync (deep-reorg replay).
    let reorg_w = make_v2_sync_log(pool, U256::from(5_000u64), U256::from(1_000u64), w, true);
    pump.run_test_loop(
        stream::iter(vec![
            header(w + 1),
            WsEvent::Pool(PoolEvent::from_log(reorg_w)),
        ])
        .boxed(),
        w,
    )
    .await;

    assert!(
        !shutdown.load(std::sync::atomic::Ordering::SeqCst),
        "a reorg inside the backfilled range is recoverable — no shutdown"
    );
    let arc = bot.state_arc();
    let core = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
    let st = core.get_v2_pool_state(pool_id).expect("v2 state");
    assert_eq!(
        st.reserve0,
        U112::from(3_000),
        "the reorg classifier restored the pre-W state (unwound W's delta)"
    );
    assert_eq!(st.reserve1, U112::from(1_500));
    assert_eq!(st.update_block, w - 5);
}

/// T4: one fact — a forward log applied to engine state —
/// feeds two consumers: the FSM quiesce arm (`on_log_applied`, which
/// arms the quiesce-gated publish) and the engine-side
/// `has_logs_this_block` bookkeeping, routed through the
/// sink's `record_logs_this_block`. This pin asserts the pairing: a
/// forward log fires exactly one `record_logs_this_block` AND arms the
/// quiesce publish (`on_send`); a reorg (`removed: true`) log fires
/// neither — the reorg arms early-return before the apply+record site.
/// Green-on-first-run pin of the status quo (no production change).
#[tokio::test]
async fn log_applied_pairing_forward_records_reorg_does_not() {
    use stream::StreamExt;

    let bot = Arc::new(Bot::new(1));
    let w = 21_500_000u64;
    let pool = Address::from([0xc2u8; 20]);
    {
        let arc = bot.state_arc();
        let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
        let _ = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: pool,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                update_block: w,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
    }

    let header = |n: u64| WsEvent::BlockHeader {
        number: n,
        timestamp: n,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    };

    // Forward: header(w+1) + a live Sync@w+1 -> applied -> both writes
    // fire (the pairing).
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);
    sink.set_dirty(true);
    pump.run_test_loop(
        stream::iter(vec![
            header(w + 1),
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(2_000u64),
                U256::from(1_500u64),
                w + 1,
                false,
            ))),
        ])
        .boxed(),
        w,
    )
    .await;
    // Sink ops are deferred to the background drainer — wait for the
    // quiesce publish to land (same pattern as
    // `decoupled_drain_still_publishes_with_block_metadata`).
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if !sink.sends().is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        sink.logs_recorded(),
        1,
        "forward log: exactly one record_logs_this_block"
    );
    assert!(
        !sink.sends().is_empty(),
        "forward log: the quiesce publish (on_log_applied's arm) fired"
    );

    // Reorg: a removed:true Sync at w+1 -> the EnterReorg arm,
    // which early-returns before the apply + record site: no record,
    // no publish.
    let (mut pump2, sink2, shutdown2) = pump_for_test_with_bot(Arc::clone(&bot), None);
    sink2.set_dirty(true);
    pump2
        .run_test_loop(
            stream::iter(vec![
                header(w + 1),
                WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                    pool,
                    U256::from(2_000u64),
                    U256::from(1_500u64),
                    w + 1,
                    true,
                ))),
            ])
            .boxed(),
            w,
        )
        .await;
    // Let the drainer settle any (nonexistent) work before asserting the
    // negative.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!shutdown2.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        sink2.logs_recorded(),
        0,
        "reorg arm: no record_logs_this_block"
    );
    assert!(
        sink2.sends().is_empty(),
        "reorg arm: no quiesce publish armed"
    );
}

/// `resume_anchors_to_subscribe_block` invariant — with no out-of-order jump from 0.
///
/// Previously named `legacy_spawn_processes_blocks_in_order_…` and framed
/// around the deleted `BlockPump::spawn` one-shot; the invariant it
/// actually pins is `resume`/`run_with_stream`'s anchoring, which survives
/// the Slice 1 deletion of `spawn` (Plan 102).
#[tokio::test]
async fn resume_anchors_to_subscribe_block() {
    // Mimic resume's first step: start with no prior cursor, then
    // `on_drain(W)` (the drain Python issues after `subscribe` returns,
    // before `resume`) anchors `last_processed_block` to W — exactly as
    // the production settle drain does (the `StageHandlers` solve stage).
    // Then resume with first_observed=W
    // (the real subscribe block, post-fix).
    let (mut pump, sink) = pump_for_test(None);
    let w = 21_500_000u64;
    let meta_w = BlockMetadata {
        timestamp: 1,
        base_fee_per_gas: Some(7),
        gas_used: 8,
        gas_limit: 9,
    };
    let meta_w1 = BlockMetadata {
        timestamp: 2,
        base_fee_per_gas: Some(10),
        gas_used: 11,
        gas_limit: 12,
    };
    let meta_w2 = BlockMetadata {
        timestamp: 3,
        base_fee_per_gas: Some(13),
        gas_used: 14,
        gas_limit: 15,
    };
    // The solve issued before resume anchors the cursor to W (SZJUKL:
    // the engine's own cursor; the dissolved coordinator cursor is gone).
    let _ = sink.on_solve(&crate::bot_core::Solve {
        ctx: BlockContext::new(w, meta_w),
        paths: crate::bot_core::AffectedPaths::default(),
    });
    assert_eq!(
        sink.last_processed_block(),
        Some(Epoch::at(w)),
        "solve(W) must anchor the cursor (mirrors the old SolveCoordinator drain)"
    );

    // Resume stream (post-fix: first_observed = W, not 0). header(W+1)
    // is the first header → first_header anchor advances W→W+1; then a
    // forward log for W+2 tombstones W+1 → finalize(W+1, meta_w1).
    let tombstone_log = make_v2_sync_log(
        Address::from([0xfcu8; 20]),
        U256::from(1),
        U256::from(2),
        w + 2,
        false,
    );
    let events: Vec<WsEvent> = vec![
        WsEvent::BlockHeader {
            number: w + 1,
            timestamp: meta_w1.timestamp,
            base_fee_per_gas: meta_w1.base_fee_per_gas,
            gas_used: meta_w1.gas_used,
            gas_limit: meta_w1.gas_limit,
        },
        WsEvent::BlockHeader {
            number: w + 2,
            timestamp: meta_w2.timestamp,
            base_fee_per_gas: meta_w2.base_fee_per_gas,
            gas_used: meta_w2.gas_used,
            gas_limit: meta_w2.gas_limit,
        },
        WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, w).await;
    drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

    // log(W+2) tombstones W+1 — carrying meta_w1 (W+1's own metadata,
    // snapshotted when header W+1 arrived). Proves the anchor held: we
    // advanced W→W+1→W+2 in order, never jumping from 0.
    let finalized = sink.finalized.lock().unwrap().clone();
    assert!(
        !finalized.is_empty(),
        "log w+2 should tombstone+finalize w+1"
    );
    assert_eq!(finalized[0].0, w + 1, "first finalize is for block w+1");
    assert_eq!(
        finalized[0].1, meta_w1,
        "block w+1's batch carries w+1's metadata (in-order)"
    );
}

/// SZJUKL port of the dissolved `event_dispatch` test
/// `drainer_warns_and_drops_reorg_flying_stale_epoch_work`: the stale-epoch
/// drop is now the DRIVER-side `reorg_flying_stale` check at each work
/// site — the `DispatchOwner` FIFO is gone. A work item minted in the
/// pre-rewind generation is dropped LOUDLY (WARN + metric) instead of
/// silently consuming `epoch.block()` into solve/finalize bookkeeping;
/// the post-rewind item minted in the bumped generation is applied.
#[test]
fn driver_drops_reorg_flying_stale_epoch_work() {
    let (pump, sink) = pump_for_test(None);
    let mut fsm = StageMachine::new(100, 0);

    // The stage machine rewinds: the generation bumps to 1 (I2) — a
    // removed log opens the window, a forward closes it.
    assert!(matches!(fsm.on_log(90, true), LogDecision::EnterReorg(_)));
    assert!(matches!(
        fsm.on_log(91, false),
        LogDecision::CloseReorg { .. }
    ));
    assert_eq!(fsm.rewind_seq(), 1);

    // Pre-rewind (reorg-flying) work: an item minted BEFORE the bump in
    // generation 0 — dropped, never applied to the engine.
    let stale_ctx = BlockContext::new(
        crate::bot_core::Epoch::with_generation(100, 0),
        BlockMetadata::default(),
    );
    assert!(pump.reorg_flying_stale(&fsm, &stale_ctx));
    pump.drive_finalize(
        &fsm,
        BlockContext::new(
            crate::bot_core::Epoch::with_generation(100, 0),
            BlockMetadata::default(),
        ),
    );
    assert!(
        sink.finalized.lock().unwrap().is_empty(),
        "the reorg-flying finalize must be dropped, never applied"
    );

    // Fresh (post-rewind) work at the bumped generation: applied normally.
    assert!(
        !pump.reorg_flying_stale(&fsm, &fsm.context_for(100, BlockMetadata::default())),
        "context_for mints at the CURRENT generation"
    );
    pump.drive_finalize(
        &fsm,
        crate::bot_core::BlockContext::new(
            crate::bot_core::Epoch::with_generation(100, 1),
            BlockMetadata::default(),
        ),
    );
    assert_eq!(sink.finalized.lock().unwrap().len(), 1);
    assert_eq!(sink.finalized.lock().unwrap().first().unwrap().0, 100);
}
