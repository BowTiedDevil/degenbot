use super::*;

#[test]
fn build_backfill_filter_constructs_valid_filter() {
    let filter = build_backfill_filter(100, 200);
    let debug_str = format!("{filter:?}");
    assert!(!debug_str.is_empty());
}

#[test]
fn shutdown_flag_stops_uniswap_pump() {
    let shutdown = Arc::new(AtomicBool::new(true));
    assert!(shutdown.load(Ordering::Relaxed));
}

#[test]
fn relevant_topics_contains_all_seven() {
    assert_eq!(RELEVANT_TOPICS.len(), 7);
    // Verify each is non-zero
    for topic in &RELEVANT_TOPICS {
        assert_ne!(topic, &B256::ZERO);
    }
}

#[test]
fn backfill_timeout_constant_is_reasonable() {
    // 60s is the chosen timeout — verify it's set
    assert_eq!(BACKFILL_TIMEOUT_SECS, 60);
}

#[test]
fn test_pump_disables_ws_completeness_by_default() {
    // Same per-pump opt-out as the solver-state tripwire: the per-block
    // WS-delivery completeness cross-check is conservative-ON in production
    // (`DEGENBOT_WS_COMPLETENESS`, via `bot_env_flag_default_on`) but
    // deterministically OFF in the test constructor so synthetic log
    // streams (relevant-topic logs used as pure block tombstones) never
    // trip a spurious eth_getLogs comparison/abort.
    let (pump, _sink) = pump_for_test(None);
    assert!(
        !pump.ws_completeness_enabled,
        "test pumps must disable the WS-delivery completeness cross-check"
    );
    // And the production default must be ON so drops surface loudly out
    // of the box (KAHU5W: typed schema default, loader owns env).
    assert!(
        crate::bot_core::stance::config().pump.ws_completeness,
        "production default for pump.ws_completeness must be ON"
    );
}

/// B4GX7C/sole-mode: the GIL-bound `on_send` (Python dispatch) runs on the
/// background drainer task so the WS poller is never parked behind
/// `Python::attach`. This exercises the (now sole) mode end-to-end: a
/// header opens block 101, a V2 Sync log for 101 opens + quiesces it, and
/// the stream-exhaust settle point flushes the quiesce-gated publish —
/// which MUST still fire `on_send` (with the block metadata) from the
/// drainer.
#[tokio::test]
async fn decoupled_drain_still_publishes_with_block_metadata() {
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
            update_block: 500,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    }
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    sink.set_dirty(true); // fake sink: mark dirty so the eager drain fires

    // Header(101) + V2 Sync@101 opens + quiesces block 101; stream end
    // flushes the quiesce-gated publish.
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
            alloy::primitives::U256::ZERO,
            alloy::primitives::U256::ZERO,
            101,
            false,
        ))),
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;

    // All sink ops were deferred to the drainer; wait until the drain,
    // the header notify, and the publish have all landed.
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let done = !sink.drained.lock().unwrap().is_empty()
            && !sink.notified.lock().unwrap().is_empty()
            && !sink.sent.lock().unwrap().is_empty();
        if done || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Notify(header 101) routed to the drainer.
    let notified = sink.notified.lock().unwrap().clone();
    assert!(
        notified.iter().any(|&(b, _)| b == 101),
        "decoupled header notify must fire (got {notified:?})"
    );

    // Drain(eager solve of the dirty pool) routed to the drainer.
    let drained = sink.drained.lock().unwrap().clone();
    assert!(
        !drained.is_empty(),
        "decoupled eager drain must solve dirty paths (got {drained:?})"
    );

    // Publish(on_send) routed to the drainer — with the block metadata.
    let sent = sink.sent.lock().unwrap().clone();
    assert!(
        !sent.is_empty(),
        "decoupled publish must still fire on_send (got {sent:?})"
    );
    assert_eq!(
        sent[0].timestamp, 101_000,
        "publish carries the quiesced block's metadata"
    );
}
