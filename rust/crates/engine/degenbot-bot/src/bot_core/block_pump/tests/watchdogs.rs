use super::*;

/// JIABO3 Option A — header-staleness watchdog independence.
///
/// Contract: a `tokio::time::interval` selected against `combined.next()`
/// wakes the pump even when the WS stream is silent (no new headers / no
/// logs after an initial header), firing `handle_timeout_eager` and
/// backfilling past the stream's observed block. This is the independence
/// the in-loop `timeout(.. combined.next())` lacked: that timeout only
/// arms once the loop body reaches its select await, and under dense-log
/// pressure `combined.next()` keeps yielding so the 60s no-activity path
/// never elapses — a silent `newHeads` goes undetected. The watchdog tick
/// elapses on its OWN internal `Sleep`, racing `combined.next()`.
///
/// Stream: header(101), then 250ms silence, then end. `header_staleness`
/// overridden to 100ms. Mock RPC: `eth_blockNumber` → 102,
/// `eth_getLogs`(102) → empty. The only way `set_last_solved_block(102)`
/// lands is the watchdog's backfill — the stream delivered only 101.
#[tokio::test]
async fn header_staleness_watchdog_fires_under_silent_stream() {
    let (mut pump, sink, asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_header_staleness_for_test(Duration::from_millis(100));

    // FIFO mock queue: the watchdog's `get_block_number` (returns 102 so
    // `latest > current` triggers backfill), then `get_logs` for block 102
    // (empty — `backfill_range` still stamps `last_processed_block=102`
    // per iteration). Extra `0x66` results pad later ticks (current already
    // 102 → `latest > current` is false → no second backfill, no `get_logs`).
    asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
    asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());
    asserter.push_success(&"0x66".to_string());

    // Stream: one header (anchors current_block=101, sets last_header_at),
    // then 250ms of silence (combined.next() stays pending → only the
    // watchdog tick can win the select), then end.
    let combined = stream::unfold(0u8, |phase| async move {
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
                None
            }
            _ => None,
        }
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    let solved = sink.solved.lock().unwrap().clone();
    assert!(
        solved.contains(&102),
        "watchdog must backfill block 102 under a silent stream; \
             set_last_solved_block calls were {solved:?}"
    );
}

/// JIABO3 Option A — guard: the watchdog does NOT spuriously fire when
/// headers keep arriving within the staleness window. The
/// `last_header_at.elapsed() >= header_staleness` guard must prevent
/// backfill under a live `newHeads` stream, even though the interval tick
/// still elapses. Locks the guard so a future regression that drops it (and
/// backfills on every tick) fails here.
#[tokio::test]
async fn header_staleness_watchdog_does_not_fire_when_headers_fresh() {
    let (mut pump, sink, asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
    // Generous margin (200ms staleness, headers every 50ms) so the test is
    // not timing-flaky: at any tick elapse, `last_header_at` is <100ms old.
    pump.set_header_staleness_for_test(Duration::from_millis(200));

    // If the watchdog fired spuriously, it would consume these and
    // backfill block 999 (way beyond the stream's observed blocks) →
    // `set_last_solved_block(999)` would land. The assertion is the
    // negative: 999 absent AND the queue unconsumed.
    asserter.push_success(&"0x3e7".to_string()); // eth_blockNumber → 999
    asserter.push_success(&Vec::<Log>::new());

    // Headers 101..105 arriving every 50ms (well within the 200ms
    // staleness window), then end at 300ms.
    let combined = stream::unfold((0u8, 101u64), |(phase, block)| async move {
        match phase {
            _ if block <= 105 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Some((
                    WsEvent::BlockHeader {
                        number: block,
                        timestamp: block,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                    },
                    (phase, block + 1),
                ))
            }
            _ => None,
        }
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    let solved = sink.solved.lock().unwrap().clone();
    assert!(
        !solved.contains(&999),
        "watchdog must NOT backfill while headers are fresh; \
             set_last_solved_block calls were {solved:?}"
    );
    assert_eq!(
        asserter.read_q().len(),
        2,
        "watchdog must not have polled the provider while headers were fresh"
    );
}

/// Logs-subscription liveness watchdog (inverse of header staleness):
/// headers keep flowing but the `eth_subscribe "logs"` arm delivers
/// NOTHING for `log_silence` → one warning per silence episode. Proves the
/// detector fires under the failure mode Alternative B's header-only
/// handshake no longer catches at startup.
#[tokio::test]
async fn logs_silence_watchdog_fires_when_headers_flow_but_no_logs() {
    let (mut pump, _sink, _asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
    // Tick every 100ms so the silence check runs often; headers fresh
    // every 40ms (well within the 100ms window); silence threshold 150ms.
    pump.set_header_staleness_for_test(Duration::from_millis(100));
    pump.set_log_silence_for_test(Duration::from_millis(150));

    // Headers 101..110 every 40ms (kept fresh), NO logs at all, then end.
    // At ~150ms `last_log_at` (anchored at start) crosses the threshold;
    // the next staleness tick (headers fresh) fires the alarm.
    let combined = stream::unfold(101u64, |block| async move {
        if block > 110 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        Some((
            WsEvent::BlockHeader {
                number: block,
                timestamp: block,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
            },
            block + 1,
        ))
    })
    .boxed();

    pump.run_test_loop(combined, 100).await;

    assert!(
        pump.log_silence_alarm_count() >= 1,
        "logs-silence alarm MUST fire when headers flow but no log arrives \
             within log_silence (got {})",
        pump.log_silence_alarm_count()
    );
}

/// Guard: the logs-silence alarm does NOT fire while logs are flowing
/// (each `WsEvent::Log` refreshes `last_log_at` and re-arms the alarm).
/// Locks the refresh path so a regression that drops it (and alarms on
/// every tick despite live logs) fails here.
#[tokio::test]
async fn logs_silence_watchdog_does_not_fire_when_logs_flowing() {
    let (mut pump, _sink, _asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
    pump.set_header_staleness_for_test(Duration::from_millis(100));
    pump.set_log_silence_for_test(Duration::from_millis(150));

    let pool = Address::from([0x11u8; 20]);
    // Header + one V2 Sync log every 40ms (both subs alive):
    // `last_log_at` never reaches 150ms. Header+log pairs for blocks 101..110, then end.
    let combined = stream::unfold((101u64, 0u8, pool), |(block, toggle, pool)| async move {
        if block > 110 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        let event = if toggle == 0 {
            WsEvent::BlockHeader {
                number: block,
                timestamp: block,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
            }
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
        pump.log_silence_alarm_count(),
        0,
        "logs-silence alarm must NOT fire while logs are flowing"
    );
}

#[tokio::test]
async fn finalize_carries_just_finished_blocks_metadata() {
    // Contract (VTWCIG, ADR-008): block N is finalized when the FIRST
    // `removed: false` LOG for N+1 arrives (the tombstone — NOT a header).
    // The result batch that finalizes N must carry N's OWN metadata, even
    // though header N+1 (with distinct metadata) arrived earlier and
    // overwrote `current_metadata`. Python computes `base_fee_next` from
    // this metadata; carrying N+1's would systematically mis-price settlement arbitrage.
    //
    // Stream: header 101, header 102 (overwrites current_metadata to
    // meta_102), then a forward log for block 102 (tombstones 101). The
    // finalize(101) must carry meta_101, NOT meta_102.
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
    // header(101): first_header anchor → current_block 101.
    // header(102): new block, current_metadata overwritten to meta_102,
    //   but NO finalize on header (ADR-008).
    // log(102, removed=false): tombstones 101 → finalize(101, meta_101).
    let tombstone_log = make_v2_sync_log(
        Address::from([0xfcu8; 20]),
        U256::from(1),
        U256::from(2),
        102,
        false,
    );
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
        WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

    let finalized = sink.finalized.lock().unwrap().clone();
    assert!(
        !finalized.is_empty(),
        "log 102 should tombstone+finalize 101"
    );
    let (block, metadata) = &finalized[0];
    assert_eq!(*block, 101, "first finalize is for block 101");
    assert_eq!(
        *metadata, meta_101,
        "block 101's batch must carry 101's metadata, not 102's"
    );
    assert_ne!(
        *metadata, meta_102,
        "block 101's batch must NOT carry 102's metadata"
    );
}

/// BO5FBS active-block promotion (QMSTSV, confirmed): the pump sets the
/// solve anchor = max(newHead-driven `current_block`, `pool_state_head`).
/// On a header stall, ordered backfill advances the state clock above
/// `current_block`; the solve anchor must never be below the state it
/// solves against (MQIZ5M +1-wei / IIA class). Here a V2 pool is
/// registered at `update_block` 500 while the pump advances headers only to
/// 103 — every `on_drain` must receive the promoted 500, not the lagging
/// header. RED before the pump-owned promotion, GREEN after.
#[tokio::test]
async fn on_drain_receives_promoted_active_block_not_stalled_header() {
    use alloy::primitives::{aliases::U112, Address as A};
    use stream::StreamExt;
    let bot = Arc::new(Bot::new(1));
    {
        let arc = bot.state_arc();
        let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.register_v2_pool(&RegisterV2PoolParams {
            address: A::from([0xabu8; 20]),
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
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pool_state_head(),
        500,
        "state clock is ahead of the header clock (the stall)"
    );
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    sink.set_dirty(true);

    let events: Vec<WsEvent> = (101..=103)
        .map(|number| WsEvent::BlockHeader {
            number,
            timestamp: number * 1_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        })
        .collect();
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    let drained = sink.drained_blocks();
    assert!(
        !drained.is_empty(),
        "dirty sink must fire on_drain each top-of-loop iteration"
    );
    assert!(
            drained.iter().all(|&b| b == 500),
            "every on_drain must receive the promoted active_block (pool_state_head 500), got {drained:?}"
        );
    assert!(
        drained.iter().all(|&b| b >= 103),
        "no on_drain may lag below the state clock: {drained:?}"
    );
}

/// Drained-settle solve gate (header form): the solve
/// fires EXACTLY ONCE, after the buffered header burst is drained, at the
/// newest observed block — never eagerly at the top of every loop
/// iteration (the old behavior dispatched one solve per buffered event and
/// lagged each header by one block).
#[tokio::test]
async fn solve_gate_waits_for_drained_stream_headers() {
    use stream::StreamExt;
    let bot = Arc::new(Bot::new(1));
    let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
    sink.set_dirty(true);

    let events: Vec<WsEvent> = (101..=103)
        .map(|number| WsEvent::BlockHeader {
            number,
            timestamp: number * 1_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        })
        .collect();
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    assert_eq!(
        sink.drained_blocks(),
        vec![103],
        "solve must fire once, after the buffered header burst drains, at the newest block"
    );
}
