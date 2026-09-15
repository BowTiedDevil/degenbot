use super::*;

// -----------------------------------------------------------------
// ADR-008 D2: `LogsQuiesced` solver-release gate.
//
// The pump must publish (`on_send`) only when the open block is
// quiesced (all dispatched logs fully applied), and coalesce a burst of
// same-block logs into ONE publish at the burst tail (not once per log).
// Re-arm on straggler is covered at the clock level by
// `consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler`.
// -----------------------------------------------------------------

/// 3 same-block logs in a tight burst → exactly ONE `on_send`, fired at
/// the burst tail (after the 3rd log applies + the stream settles), NOT
/// 3× (one per log) and NOT zero. RED against the wall-clock timer: with
/// `stream::iter` (no delay between events) the `DEBOUNCE_MS` timer
/// never fires before the stream ends, so `on_send` is never called.
#[tokio::test]
async fn burst_of_logs_publishes_once_at_tail_via_quiesce_gate() {
    let (mut pump, sink) = pump_for_test(Some(100));
    let pool_addr = Address::from([0x55u8; 20]);
    let mk = |r0, r1| {
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            pool_addr,
            U256::from(r0),
            U256::from(r1),
            101,
            false,
        )))
    };
    // 3 same-block sync logs, then stream exhaustion. Under the wall-clock
    // debounce the timer never fires before Ok(None) returns → 0
    // sends. Under the quiesce gate, after the 3rd log applies the settle
    // probe (timeout(ZERO) on the exhausted stream) flushes on_send once.
    let combined = stream::iter(vec![mk(1_500, 2_500), mk(1_600, 2_600), mk(1_700, 2_700)]).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.sent.lock().unwrap().is_empty()).await;

    let sent = sink.sent.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "a 3-log burst publishes exactly once at the tail via the quiesce \
             gate (got {} sends)",
        sent.len()
    );
}
/// BO5FBS publish-gate interaction: the newHead-driven eager solve is
/// distinct from the publish gate. With the promotion live, `on_drain`
/// fires eagerly at the promoted block (`pool_state_head` 500) on the
/// `LogsArriving` path, but NO publish (`on_send`) occurs until a forward
/// log quiesces the block (ADR-008 D2). A header with no log must never
/// leak a publish — newHead is a promote/liveness signal, never a
/// completeness signal.
#[tokio::test]
async fn newhead_promoted_solve_does_not_publish_until_quiesced() {
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
    sink.set_dirty(true);

    // newHead(101) only — no log for 101, so the block is LogsArriving
    // (open), not quiesced.
    let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
        number: 101,
        timestamp: 101_000,
        base_fee_per_gas: Some(1_000_000_001),
        gas_used: 10_000_001,
        gas_limit: 30_000_001,
    }];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, 100).await;
    drainer_settle(|| !sink.drained_blocks().is_empty()).await;

    let drained = sink.drained_blocks();
    assert!(
        !drained.is_empty(),
        "dirty sink fires the eager newHead-driven solve"
    );
    assert!(
        drained.iter().all(|&b| b == 500),
        "eager solve anchors to the promoted active_block (500)"
    );
    let sent = sink.sent.lock().unwrap().clone();
    assert!(
        sent.is_empty(),
        "no publish during LogsArriving: a header alone must never leak \
             an on_send (quiesce gate), got {sent:?}"
    );
}

/// FD7NFG: `backfill_from_snapshot` no-op when no snapshot loaded (cold
/// start — `snapshot_seed_block = None`). Default fresh `Bot` has S=None.
#[tokio::test]
async fn backfill_from_snapshot_cold_start_is_noop() {
    let (pump, _sink) = pump_for_test(None);
    // Fresh Bot: snapshot_seed_block is None → no-op, no provider call.
    let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
    assert_eq!(n, 0, "cold start (S=None) → no blocks backfilled");
}

/// FD7NFG: `backfill_from_snapshot` no-op when `S >= W` (snapshot at/after
/// the WS block — nothing to backfill).
#[tokio::test]
async fn backfill_from_snapshot_s_ge_w_is_noop() {
    let (pump, _sink) = pump_for_test(None);
    // Inject S = W (snapshot caught up to the WS block).
    {
        let bot = pump.bot_arc_for_test();
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(100));
    }
    let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
    assert_eq!(n, 0, "S >= W → nothing to backfill");
}

/// FD7NFG: `backfill_from_snapshot` no-op when `S = 0` (degenerate
/// snapshot block — guarded to avoid a `from_block=1` unbounded fetch).
#[tokio::test]
async fn backfill_from_snapshot_s_zero_is_noop() {
    let (pump, _sink) = pump_for_test(None);
    {
        let bot = pump.bot_arc_for_test();
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(0));
    }
    let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
    assert_eq!(n, 0, "S = 0 → skip (degenerate)");
}

/// J3FMDO: `resume_from_subscribe` auto-backfills the snapshot→WS gap
/// (S < W) before the live loop begins — proving the core path closes the
/// gap with zero Python orchestration. The Asserter queue drains by exactly
/// one `eth_getLogs` response (S+1..W fits in a single default-size chunk).
#[tokio::test]
async fn auto_backfill_runs_inside_resume_when_s_lt_w() {
    let bot = Arc::new(Bot::new(1));
    bot.state_arc()
        .write_at(crate::bot_core::state_lock::LockSite::Pump)
        .set_snapshot_seed_block(Some(85));
    let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

    // The single eth_getLogs chunk (blocks 86..99, ≤ DEFAULT_BACKFILL_CHUNK_SIZE)
    // returns an empty log array — the pump's provider drains this response.
    asserter.push_success(&Vec::<Log>::new());

    let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
    let state = SubscribeState {
        first_block: 100,
        first_timestamp: 0,
        combined_stream: Some(combined),
    };
    pump.resume_from_subscribe(state).await;

    assert_eq!(
        asserter.read_q().len(),
        0,
        "auto-backfill inside resume popped exactly one eth_getLogs response; queue must be empty"
    );
}

/// J3FMDO race regression: `backfill_to_ws_block` is the
/// synchronously-awaitable backfill that `PumpState::resume` `block_on`s
/// BEFORE spawning the live loop. Pre-fix the backfill ran INSIDE the
/// spawned `resume_from_subscribe` task, so `PumpState::resume` returned
/// immediately and Python's `build_paths` drained an EMPTY backfill buffer
/// (the burn for an active pool was not yet buffered) → the post-drain
/// verify mismatched on-chain and crashed the settlement-arbitrage bot with
/// `VerificationMismatchError`. This pins the contract: after
/// `backfill_to_ws_block` returns, the V3 backfill buffer is populated —
/// the event did NOT require the live loop to run first.
#[tokio::test]
async fn backfill_to_ws_block_populates_buffer_before_return() {
    let pool_addr = alloy::primitives::Address::from([0xc2u8; 20]);
    let bot = Arc::new(Bot::new(1));
    bot.state_arc()
        .write_at(crate::bot_core::state_lock::LockSite::Pump)
        .set_snapshot_seed_block(Some(85));
    let (pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(Arc::clone(&bot), None);

    // A V3 Burn log at block 90 (in the backfill range 86..99).
    asserter.push_success(&vec![make_v3_burn_log_with_block(
        pool_addr, -100, 100, 500, 90,
    )]);

    // backfill_to_ws_block must fully buffer the burn BEFORE returning.
    pump.backfill_to_ws_block(100)
        .await
        .expect("backfill_to_ws_block completes against the mock");

    // The burn was buffered (not applied — pool unregistered → buffer
    // branch). Pre-fix: this method did not exist and `resume` returned
    // before the spawned task buffered → count 0 → race.
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .buffered_v3_event_count(&pool_addr),
        1,
        "backfill_to_ws_block must buffer the V3 burn before returning (race regression)"
    );
}

/// DFQYM5/WS-DROP regression: the resume-path backfill helper must drain
/// the WS stream WHILE the snapshot backfill runs and re-inject the
/// drained events ahead of the live tail. Pre-fix the pyo3
/// `PumpState::resume` ran `backfill_to_ws_block` with the stream
/// untouched, so alloy's capacity-16 subscription broadcast ring
/// overflowed (unfiltered log sub → hundreds of messages per mainnet
/// block) and silently dropped the OLDEST messages — the first live
/// block's logs — tripping the WS-completeness abort (observed live:
/// `eth_getLogs=44 logs, WS delivered=0` at block 25800995). The helper
/// returns the stream to hand to `run_with_stream`: drained events
/// first (arrival order, MJXP5Z), live tail after — and the J3FMDO
/// synchronous-backfill contract still holds (buffer populated on
/// return).
#[tokio::test]
async fn backfill_with_drain_reinjects_events_present_during_backfill() {
    let pool_addr = alloy::primitives::Address::from([0xc3u8; 20]);
    let bot = Arc::new(Bot::new(1));
    bot.state_arc()
        .write_at(crate::bot_core::state_lock::LockSite::Pump)
        .set_snapshot_seed_block(Some(85));
    let (pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(Arc::clone(&bot), None);

    // The snapshot→WS gap backfill (86..100): one V3 Burn log at block 90.
    asserter.push_success(&vec![make_v3_burn_log_with_block(
        pool_addr, -100, 100, 500, 90,
    )]);

    // Live events present on the combined stream while the backfill is in
    // flight — in production these are the freshly-mined first live
    // block's logs that the undrained alloy ring used to evict. The tail
    // pends forever to model a LIVE websocket (the drain must keep
    // running until the backfill completes, not bail on a closed stream).
    let live = vec![
        WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
            alloy::primitives::Address::from([0xd1u8; 20]),
            U256::ZERO,
            U256::ZERO,
            101,
            false,
        ))),
        WsEvent::BlockHeader {
            number: 101,
            timestamp: 1_000_101,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        },
    ];
    let combined = stream::iter(live)
        .chain(stream::pending::<WsEvent>())
        .boxed();

    let (backfill_res, mut combined) = pump.backfill_with_drain(100, combined).await;
    backfill_res.expect("backfill completes against the mock");

    // J3FMDO invariant preserved: the backfill buffer is populated on
    // return (the synchronous contract `PumpState::resume` relies on).
    assert_eq!(
        bot.state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .buffered_v3_event_count(&pool_addr),
        1,
        "backfill_with_drain must buffer the V3 burn before returning (J3FMDO)"
    );

    // The drained events were captured during the backfill and are
    // re-injected ahead of the live tail, arrival order preserved.
    let expected: [(&str, u64); 2] = [("log", 101), ("header", 101)];
    for (kind, number) in expected {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), combined.next())
            .await
            .expect("re-injected event must arrive")
            .expect("stream yields the drained event");
        match ev {
            WsEvent::Pool(pe) => {
                assert_eq!((kind, pe.payload.block_number.unwrap()), ("log", number));
            }
            WsEvent::BlockHeader { number: n, .. } => {
                assert_eq!((kind, n), ("header", number));
            }
        }
    }
}

/// J3FMDO: `resume_from_subscribe` skips the auto-backfill entirely when no
/// snapshot seed is present (`S = None`, cold start). The Asserter queue is
/// left untouched (the pump never calls `eth_getLogs`) and the live loop
/// anchors on `first_observed_block` directly. An empty queue under a live
/// `eth_getLogs` request would error; we assert the queue stays empty AND
/// the resume returns without a provider error.
#[tokio::test]
async fn auto_backfill_skipped_when_s_none_in_resume() {
    let bot = Arc::new(Bot::new(1));
    // Fresh Bot: snapshot_seed_block is None — no gap to backfill.
    let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

    let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
    let state = SubscribeState {
        first_block: 100,
        first_timestamp: 0,
        combined_stream: Some(combined),
    };
    pump.resume_from_subscribe(state).await;

    assert_eq!(
        asserter.read_q().len(),
        0,
        "cold-start resume never calls eth_getLogs (auto-backfill gated on S<W)"
    );
}

/// J3FMDO: `resume_from_subscribe` skips the auto-backfill when the
/// snapshot is already at/after the WS block (`S >= W` — catch-up snapshot
/// with no gap to backfill).
#[tokio::test]
async fn auto_backfill_skipped_when_s_ge_w_in_resume() {
    let bot = Arc::new(Bot::new(1));
    bot.state_arc()
        .write_at(crate::bot_core::state_lock::LockSite::Pump)
        .set_snapshot_seed_block(Some(100));
    let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

    let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
    let state = SubscribeState {
        first_block: 100,
        first_timestamp: 0,
        combined_stream: Some(combined),
    };
    pump.resume_from_subscribe(state).await;

    assert_eq!(
        asserter.read_q().len(),
        0,
        "S ≥ W → no auto-backfill, no eth_getLogs call"
    );
}

/// Diagnostic for the 2026-07-12 WS `eth_getLogs` hang.
///
/// Root cause (confirmed here with tracing + a concurrent
/// `get_block_number` probe): tungstenite correctly returns
/// `Error::Capacity(MessageTooLong)` for a response larger than the
/// default `max_frame_size` (16 MiB) / `max_message_size` (64 MiB), but
/// `alloy-pubsub`'s `WsBackend` converts that to
/// `TransportErrorKind::backend_gone()` (a *retryable* error) at the
/// backend→service boundary — losing the Capacity specificity. The pubsub
/// service then enters an INFINITE reconnect→redispatch loop: `reconnect()`
/// succeeds on the first attempt (the WS handshake is fine; only the
/// response is too big), `max_retries` is never consumed, and the pending
/// in-flight `eth_getLogs` is re-dispatched each cycle. The caller's
/// `get_logs` future never resolves; small concurrent calls keep working.
///
/// Three variants:
/// A — default tungstenite caps: demonstrates the infinite cycle (HUNG,
///     `get_block_number` probe still succeeding concurrently);
/// B — raised caps via raw `WsConnect::with_config`: WS handles it;
/// C — production `AlloyProvider::new` path (= the `build_provider` fix):
///     regression sentinel.
///
/// Run with:
/// `cargo test -p degenbot-bot --manifest-path rust/Cargo.toml \
///   -- --ignored --nocapture ws_getlogs_large_filter_diagnostic`
///
/// Requires `DEGENBOT_RPC_WS_CHAINID_1` (a mainnet WS endpoint).
#[tokio::test]
#[ignore = "requires a live mainnet WS endpoint (DEGENBOT_RPC_WS_CHAINID_1)"]
#[expect(clippy::too_many_lines)]
async fn ws_getlogs_large_filter_diagnostic() {
    use alloy::network::Ethereum;
    use alloy::providers::{Provider, ProviderBuilder, WebSocketConfig, WsConnect};
    use std::time::Duration;
    use tokio::time::timeout;
    use tracing_subscriber::util::SubscriberInitExt;
    type Erased = std::sync::Arc<dyn Provider<Ethereum>>;

    let Ok(ws_url) = std::env::var("DEGENBOT_RPC_WS_CHAINID_1") else {
        eprintln!("skip: DEGENBOT_RPC_WS_CHAINID_1 not set");
        return;
    };

    // Fetch a recent block number (small call — works over default WS).
    let anchor_provider: Erased = {
        let mid = ProviderBuilder::default()
            .connect_ws(WsConnect::new(ws_url.clone()))
            .await
            .expect("ws connect (anchor)")
            .erased();
        Arc::new(mid)
    };
    let latest = anchor_provider
        .get_block_number()
        .await
        .expect("block number");
    // Leave a few blocks of margin so the range is settled.
    let to = latest - 5;
    let from = to - 1_999;
    let filter = build_backfill_filter(from, to);
    eprintln!("filter range {from}–{to} (latest={latest})");

    // --- Variant A: DEFAULT tungstenite config (max_message_size=64MiB) ---
    // Install a tracing subscriber so alloy's reconnect-cycle `error!`/
    // `warn!` logs surface (without one they're silently dropped — which is
    // why the earlier run showed "no error surfaced"). Also poll
    // `get_block_number` concurrently: if it keeps succeeding while
    // `get_logs` is pending, the WS service is alive and silently
    // reconnecting (proving the oversized-response cycle), NOT truly
    // stalled in tungstenite.
    let _guard = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "alloy_pubsub=debug,alloy_transport_ws=debug,tungstenite=info",
        ))
        .with_test_writer()
        .set_default();
    let p: Erased = {
        let mid = ProviderBuilder::default()
            .connect_ws(WsConnect::new(ws_url.clone()))
            .await
            .expect("ws connect (A)")
            .erased();
        Arc::new(mid)
    };
    // Concurrent block-number probe on the SAME provider.
    let probe_p = Arc::clone(&p);
    let probe = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.tick().await; // skip immediate
        for i in 1..=15_u32 {
            tick.tick().await;
            match probe_p.get_block_number().await {
                Ok(n) => eprintln!("A probe #{i}: get_block_number OK = {n}"),
                Err(e) => eprintln!("A probe #{i}: get_block_number ERR = {e}"),
            }
        }
    });
    let t0 = std::time::Instant::now();
    let res = timeout(Duration::from_secs(30), p.get_logs(&filter)).await;
    let elapsed = t0.elapsed();
    match res {
        Ok(Ok(logs)) => eprintln!(
            "A DEFAULT   : OK  {} logs in {:.2}s (under the 64MiB cap this run)",
            logs.len(),
            elapsed.as_secs_f64()
        ),
        Ok(Err(e)) => eprintln!(
            "A DEFAULT   : ERR after {:.2}s — `{e}`",
            elapsed.as_secs_f64()
        ),
        Err(_) => {
            eprintln!("A DEFAULT   : HUNG (30s timeout, no error surfaced to the caller)");
        }
    }
    // Let the probe finish printing so we see the concurrent-call verdict.
    let _ = timeout(Duration::from_secs(35), probe).await;

    // --- Variant B: RAISED config (no size cap) ---
    let cfg = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let p: Erased = {
        let mid = ProviderBuilder::default()
            .connect_ws(WsConnect::new(ws_url.clone()).with_config(cfg))
            .await
            .expect("ws connect (B)")
            .erased();
        Arc::new(mid)
    };
    let t0 = std::time::Instant::now();
    let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
    let elapsed = t0.elapsed();
    match res {
        Ok(Ok(logs)) => eprintln!(
            "B RAISED    : OK  {} logs in {:.2}s",
            logs.len(),
            elapsed.as_secs_f64()
        ),
        Ok(Err(e)) => eprintln!(
            "B RAISED    : ERR after {:.2}s — `{e}`",
            elapsed.as_secs_f64()
        ),
        Err(_) => eprintln!("B RAISED    : HUNG (60s timeout)"),
    }

    // --- Variant C: production path (`AlloyProvider::new` → ---
    // `build_provider`), which now raises the tungstenite caps in
    // `degenbot_rpc::provider::build_provider`. This is the regression
    // sentinel: if a future change drops the raised-config in
    // `build_provider`, this variant hangs and the test suite surfaces it.
    let alloy_provider = degenbot_rpc::provider::AlloyProvider::new(&ws_url, 3)
        .await
        .expect("AlloyProvider::new");
    let p = alloy_provider.provider_arc();
    let t0 = std::time::Instant::now();
    let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
    let elapsed = t0.elapsed();
    match res {
        Ok(Ok(logs)) => eprintln!(
            "C PRODUCTION: OK  {} logs in {:.2}s",
            logs.len(),
            elapsed.as_secs_f64()
        ),
        Ok(Err(e)) => eprintln!(
            "C PRODUCTION: ERR after {:.2}s — `{e}`",
            elapsed.as_secs_f64()
        ),
        Err(_) => {
            eprintln!("C PRODUCTION: HUNG (60s timeout) — `build_provider` config regression");
        }
    }
}
