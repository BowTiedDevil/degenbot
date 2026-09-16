use super::*;

#[tokio::test(start_paused = true)]
async fn early_slice_fires_mid_burst_then_settles() {
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let bot = Arc::new(Bot::new(1));
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    pump.set_early_slice_ms_for_test(25);
    sink.set_dirty(true);

    let combined = gap_burst_stream(3, 40);
    let t0 = tokio::time::Instant::now();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| sink.drained_blocks().len() >= 2).await;

    assert_eq!(
        sink.drained_blocks(),
        vec![101, 101],
        "slice fires at the deadline mid-burst, the tail settles at the newest block"
    );
    let stamps = sink.drained_at();
    assert_eq!(stamps.len(), 2, "exactly slice + tail dispatches");
    let first_rel = stamps[0] - t0;
    assert!(
        first_rel >= Duration::from_millis(20) && first_rel <= Duration::from_millis(45),
        "slice must dispatch at ~first-dirty + 25ms, got {first_rel:?}"
    );
    let second_rel = stamps[1] - t0;
    assert!(
        second_rel >= Duration::from_millis(75),
        "tail settle must fire at burst end, got {second_rel:?}"
    );
}

/// bounded: ONE early slice per block window, however long
/// the burst (MBNASQ's unbounded per-gap serial solves must not return).
/// Six gapped headers → exactly slice + tail, never a third dispatch.
#[tokio::test(start_paused = true)]
async fn early_slice_fires_at_most_once_per_window() {
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    pump.set_early_slice_ms_for_test(25);
    sink.set_dirty(true);

    let combined = gap_burst_stream(5, 40);
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| sink.drained_blocks().len() >= 2).await;

    assert_eq!(
        sink.drained_blocks(),
        vec![101, 101],
        "exactly one early slice + one quiesce tail solve for the window"
    );
}

/// parity: `DEGENBOT_EARLY_SLICE_MS=0` disables the slice
/// entirely — the same gapped burst produces exactly the pre-T2 gate
/// behavior (one quiesce solve at stream end, timed by the full debounce).
#[tokio::test(start_paused = true)]
async fn early_slice_disabled_restores_gate_parity() {
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    pump.set_early_slice_ms_for_test(0);
    sink.set_dirty(true);

    let combined = gap_burst_stream(3, 40);
    let t0 = tokio::time::Instant::now();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    assert_eq!(
        sink.drained_blocks(),
        vec![101],
        "slice disabled: exactly the pre-T2 quiesce solve, at the newest block"
    );
    let stamps = sink.drained_at();
    assert_eq!(stamps.len(), 1, "single late dispatch, no early slice");
    let rel = stamps[0] - t0;
    assert!(
        rel >= Duration::from_millis(120),
        "slice disabled: the solve must wait out the full debounce/quiesce, got {rel:?}"
    );
}

/// adaptive quiesce: with `quiesce_mode = adaptive` (pre-seed
/// window = the 20 ms ceiling) the drained-settle gate arms the
/// ESTIMATOR window, not the fixed 50 ms debounce: the same 40 ms-gapped
/// burst that settles at ≥ 120 ms under the fixed debounce (see
/// `early_slice_disabled_restores_gate_parity`) dispatches at its
/// window deadline — because each 40 ms inter-log gap exceeds the
/// 20 ms window, exactly one dispatch fires per gap, all at block 101.
#[tokio::test(start_paused = true)]
async fn adaptive_quiesce_arms_the_estimator_window() {
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    pump.set_early_slice_ms_for_test(0);
    pump.set_quiesce_for_test(QuiesceParams {
        mode: QuiesceMode::Adaptive,
        ..QuiesceParams::default()
    });
    // The fake engine's dirty flag is a static test toggle, so raise it
    // at 35 ms VIRTUAL time — after the header, just before the first
    // log lands at 40 ms — so every drain dispatch is attributable to
    // the settle window alone (a pre-header dirty would dispatch at the
    // window deadline with no logs at all).
    {
        let sink_flag = Arc::clone(&sink);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(35)).await;
            sink_flag.set_dirty(true);
        });
    }

    let combined = gap_burst_stream(3, 40);
    let t0 = tokio::time::Instant::now();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    let stamps = sink.drained_at();
    assert!(
        !stamps.is_empty(),
        "adaptive mode must still settle (the gate never starves)"
    );
    let rel_first = stamps[0] - t0;
    // The estimator window (pre-seed = ceil 20 ms) must beat the fixed
    // debounce's earliest possible dispatch under this stream shape
    // (3 gaps × 40 ms + anything ≥ the 20 ms window): the fixed-50 ms
    // posture dispatches at ≥ 120 ms.
    assert!(
            rel_first < Duration::from_millis(120),
            "adaptive settle dispatched at {rel_first:?}; expected inside the estimator window (< 120 ms fixed-debounce floor)"
        );
    assert!(
        rel_first >= Duration::from_millis(40),
        "adaptive settle must still wait its window; dispatched at {rel_first:?}"
    );
    for (i, b) in sink.drained_blocks().iter().enumerate() {
        assert_eq!(*b, 101, "all dispatches settle the open block (drain #{i})");
    }
}

/// WAJEQP T-R1: the reorg window span lifecycle. A `removed:true` log for
/// the current block opens exactly ONE `degenbot.reorg.window` span (own
/// root); each subsequent event adds a `degenbot.reorg.restore` child;
/// the closing forward log records `reorg.new_head` + counters +
/// `reorg.outcome=closed` and ends the span. The real restore is
/// `restored`; the replay duplicate is labeled `idempotent_noop`.
#[test]
#[expect(clippy::too_many_lines)]
fn reorg_window_span_lifecycle_enter_restore_close() {
    use alloy::primitives::{Address as A, U256};
    let capture = ReorgSpanCapture::default();
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    let pool = A::from([0xccu8; 20]);
    let events = vec![
        WsEvent::BlockHeader {
            number: 101,
            timestamp: 101_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        },
        // Forward apply at 101: journal delta at 101.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(1_000),
            U256::from(2_000),
            101,
            false,
        ))),
        // Removed at 101 → EnterReorg, journal pops the 101 delta.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(1_000),
            U256::from(2_000),
            101,
            true,
        ))),
        // Duplicate removed replay → ContinueReorg, idempotent no-op.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(1_000),
            U256::from(2_000),
            101,
            true,
        ))),
        // First forward above the window → CloseReorg{102}.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(900),
            U256::from(1_800),
            102,
            false,
        ))),
    ];
    run_reorg_stream(capture.clone(), &mut pump, events);

    let spans = capture
        .spans
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let fields = capture
        .fields
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let windows: Vec<(u64, Option<u64>)> = spans
        .iter()
        .filter(|(n, _, _)| n == "degenbot.reorg.window")
        .map(|(_, id, p)| (*id, *p))
        .collect();
    assert_eq!(
        windows.len(),
        1,
        "exactly one window span per episode; got {:?}",
        spans.iter().map(|(n, _, _)| n).collect::<Vec<_>>()
    );
    let (window_id, window_parent) = windows[0];
    assert!(window_parent.is_none(), "window must be its own trace root");
    let restores: Vec<(u64, Option<u64>)> = spans
        .iter()
        .filter(|(n, _, _)| n == "degenbot.reorg.restore")
        .map(|(_, id, p)| (*id, *p))
        .collect();
    assert_eq!(restores.len(), 2, "one restore span per removed event");
    for (_id, parent) in &restores {
        assert_eq!(*parent, Some(window_id), "restore must parent the window");
    }
    // First restore: a real journal pop. Second: idempotent replay.
    let actions = || -> Vec<(u64, String)> {
        restores
            .iter()
            .filter_map(|(id, _)| {
                fields
                    .iter()
                    .find(|(sid, k, _)| sid == id && k == "reorg.action")
                    .map(|(_, _, v)| (*id, v.clone()))
            })
            .collect()
    };
    let actions = actions();
    assert!(
        actions.iter().any(|(_, v)| v == "restored"),
        "first removed event must restore: {actions:?}"
    );
    assert!(
        actions.iter().any(|(_, v)| v == "idempotent_noop"),
        "the replay duplicate must be idempotent: {actions:?}"
    );
    let window_fields = |want: &str| -> Option<String> {
        fields
            .iter()
            .find(|(sid, k, _)| *sid == window_id && k == want)
            .map(|(_, _, v)| v.clone())
    };
    assert_eq!(window_fields("reorg.outcome").as_deref(), Some("closed"));
    assert_eq!(window_fields("reorg.new_head").as_deref(), Some("102"));
    assert_eq!(window_fields("reorg.pools_restored").as_deref(), Some("1"));
    assert_eq!(
        window_fields("reorg.idempotent_noops").as_deref(),
        Some("1")
    );
    // Breadcrumbs on the interrupted block's epoch root span.
    let block_spans: Vec<u64> = spans
        .iter()
        .filter(|(n, _, _)| n == "degenbot.epoch.run")
        .map(|(_, id, _)| *id)
        .collect();
    assert!(!block_spans.is_empty());
    let bs_id = block_spans[0];
    assert_eq!(
        fields
            .iter()
            .find(|(sid, k, _)| *sid == bs_id && k == "reorg.entry_block")
            .map(|(_, _, v)| v.as_str()),
        Some("101")
    );
    assert_eq!(
        fields
            .iter()
            .find(|(sid, k, _)| *sid == bs_id && k == "reorg.closed")
            .map(|(_, _, v)| v.as_str()),
        Some("102")
    );
}

/// WAJEQP T-R1: a removed log for a pool whose newest journal delta is
/// already below the target restores nothing — the restore span is still
/// emitted, labeled `idempotent_noop`, and the window still closes.
#[test]
fn reorg_restore_without_delta_is_idempotent_noop() {
    use alloy::primitives::{Address as A, U256};
    let capture = ReorgSpanCapture::default();
    let bot = Arc::new(Bot::new(1));
    register_burst_pool(&bot);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    let pool = A::from([0xccu8; 20]);
    let events = vec![
        WsEvent::BlockHeader {
            number: 101,
            timestamp: 101_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        },
        // No forward apply at 101: the newest delta is the registration
        // (block 100), so this removed event is a guaranteed no-op.
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(1_000),
            U256::from(2_000),
            101,
            true,
        ))),
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            U256::from(900),
            U256::from(1_800),
            102,
            false,
        ))),
    ];
    run_reorg_stream(capture.clone(), &mut pump, events);

    let spans = capture
        .spans
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let fields = capture
        .fields
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let restores: Vec<u64> = spans
        .iter()
        .filter(|(n, _, _)| n == "degenbot.reorg.restore")
        .map(|(_, id, _)| *id)
        .collect();
    assert_eq!(restores.len(), 1);
    assert_eq!(
        fields
            .iter()
            .find(|(sid, k, _)| sid == &restores[0] && k == "reorg.action")
            .map(|(_, _, v)| v.as_str()),
        Some("idempotent_noop")
    );
    assert_eq!(
        fields
            .iter()
            .find(|(_, k, _)| k == "reorg.outcome")
            .map(|(_, _, v)| v.as_str()),
        Some("closed")
    );
}

/// TQ7PD6 follow-up — drained-settle solve gate (log form): the solve must
/// NOT fire before a still-buffered log for the block is applied. Header
/// 101 + V2 Sync@101 are delivered back-to-back; the old loop-head solve
/// dispatched at block 100 (the pre-log anchor) before consuming the log.
/// The gate defers until both events are drained, then solves at 101 — the
/// freshest block, with the swap applied.
#[tokio::test]
async fn solve_gate_waits_for_buffered_log_before_solving() {
    use alloy::primitives::{aliases::U112, Address as A};
    use stream::StreamExt;
    let bot = Arc::new(Bot::new(1));
    {
        let arc = bot.state_arc();
        let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.register_v2_pool(&RegisterV2PoolParams {
            address: A::from([0xccu8; 20]),
            token0: A::from([0xa0u8; 20]),
            token1: A::from([0xa1u8; 20]),
            reserve0: U112::from(1_000),
            reserve1: U112::from(2_000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: A::from([0xf0u8; 20]),
            update_block: 100,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    }
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    sink.set_dirty(true);

    let pool = A::from([0xccu8; 20]);
    let events: Vec<WsEvent> = vec![
        WsEvent::BlockHeader {
            number: 101,
            timestamp: 101_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        },
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool,
            alloy::primitives::U256::from(1_000),
            alloy::primitives::U256::from(2_000),
            101,
            false,
        ))),
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    assert_eq!(
        sink.drained_blocks(),
        vec![101],
        "solve must fire only after the buffered Sync log is applied (fresh block)"
    );
}
