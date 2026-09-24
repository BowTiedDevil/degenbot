//! Backrun feed client spec.
//!
//! Seam: `degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig, BackrunFeedEvent}`
//! and `BackrunFeedStatus`. Tests drive a local mock WS server and assert on
//! wire behavior: the subscribe handshake, typed event parse, chain-id gate,
//! reconnect-on-drop, silent-socket watchdog, ring eviction, parse resilience.

#![expect(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex, Once, OnceLock};
use std::time::{Duration, Instant};

use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

const SUB_METHOD: &str = "mevblocker_partialPendingTransactions";
const ACK: &str = r#"{"jsonrpc":"2.0","id":1,"result":"0xsubid"}"#;

enum Step {
    /// Await + assert the named subscribe handshake, record it.
    ExpectSubscribe,
    Ack,
    Push(serde_json::Value),
    Close,
    /// Park reading until the peer disconnects.
    Hold,
}

fn sample_tx(chain_id: &str, nonce: u64, hash: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "method": "eth_subscription",
        "params": {
            "subscription": "0xsubid",
            "result": {
                "chainId": chain_id,
                "to": "0x2222222222222222222222222222222222222222",
                "value": "0x4fefa17b724000",
                "data": "0xdeadbeef",
                "accessList": [],
                "nonce": format!("0x{nonce:02x}"),
                "maxPriorityFeePerGas": "0x0",
                "maxFeePerGas": "0x7e1c65b04",
                "gas": "0x5208",
                "type": "0x2",
                "hash": hash,
                "from": "0x1111111111111111111111111111111111111111"
            }
        }
    })
}

fn sample_event(chain_id: &str) -> serde_json::Value {
    sample_tx(
        chain_id,
        0,
        "0x5f08dd372fce1a44dda27bed60ca036acb4979fad6ca37b9c388e351a870fe4c",
    )
}

fn cfg(port: u16) -> BackrunFeedConfig {
    BackrunFeedConfig {
        url: format!("ws://127.0.0.1:{port}"),
        ..BackrunFeedConfig::for_mainnet()
    }
}

fn cfg_with(port: u16, watchdog: Duration, ring: usize) -> BackrunFeedConfig {
    BackrunFeedConfig {
        url: format!("ws://127.0.0.1:{port}"),
        watchdog,
        ring_capacity: ring,
        reconnect_backoff: Duration::from_millis(20),
        ..BackrunFeedConfig::for_mainnet()
    }
}

/// Spawn a feed on a fresh per-test hub (one source per hub).
fn spawn_feed(cfg: BackrunFeedConfig) -> BackrunFeed {
    let hub = degenbot_eventhub::Hub::new();
    BackrunFeed::spawn_on_hub(&hub, cfg).expect("fresh hub registers the pending feed")
}

struct MockServer {
    addr: SocketAddr,
    seen: Arc<StdMutex<Vec<String>>>,
    _finish: tokio::sync::oneshot::Sender<()>,
}

async fn serve_conn<S>(
    mut ws: WebSocketStream<S>,
    steps: Vec<Step>,
    seen: Arc<StdMutex<Vec<String>>>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::future::Either;
    for step in steps {
        match step {
            Step::ExpectSubscribe => {
                let fut = ws.next();
                match tokio::time::timeout(Duration::from_secs(5), fut).await {
                    Ok(Some(Ok(Message::Text(t)))) => {
                        seen.lock().unwrap().push(t.to_string());
                        assert!(
                            t.contains("eth_subscribe") && t.contains(SUB_METHOD),
                            "handshake must request {SUB_METHOD}, got: {t}"
                        );
                    }
                    other => panic!("expected subscribe frame, got {other:?}"),
                }
            }
            Step::Ack => ws.send(Message::Text(ACK.into())).await.unwrap(),
            Step::Push(v) => ws.send(Message::Text(v.to_string().into())).await.unwrap(),
            Step::Close => {
                let _ = ws.send(Message::Close(None)).await;
                return;
            }
            Step::Hold => {
                // Park until shutdown signal or peer disconnect.
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    while let Some(Ok(_)) = ws.next().await {}
                })
                .await;
                let _ = Either::<(), ()>::Left(());
                return;
            }
        }
    }
}

async fn start_server(scripts: Vec<Vec<Step>>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = finish_rx.await;
    });
    let mut scripts = scripts.into_iter();
    let seen_cl = seen.clone();
    tokio::spawn(async move {
        let mut idx = 0usize;
        let mut pending: Option<Vec<Step>> = None;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            // No scripted connection left: fall through and drop the socket.
            let Some(script) = pending.take().or_else(|| scripts.next()) else {
                continue;
            };
            let _ = idx;
            idx += 1;
            let seen = seen_cl.clone();
            tokio::spawn(async move {
                let Ok(ws) = accept_async(stream).await else {
                    return;
                };
                serve_conn(ws, script, seen).await;
            });
        }
    });
    MockServer {
        addr,
        seen,
        _finish: finish_tx,
    }
}

fn seen(server: &MockServer) -> Vec<String> {
    server.seen.lock().unwrap().clone()
}

async fn drain_until(
    feed: &BackrunFeed,
    want: usize,
    timeout: Duration,
) -> Vec<degenbot_rpc::backrun_feed::BackrunFeedEvent> {
    let mut acc: Vec<_> = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        acc.extend(feed.drain());
        if acc.len() >= want {
            return acc;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want} events (got {})",
            acc.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn t01_subscribe_handshake_requests_named_subscription_and_drains_typed_event() {
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Push(sample_event("0x1")),
    ]])
    .await;
    let feed = spawn_feed(cfg(server.addr.port()));
    let evs = drain_until(&feed, 1, Duration::from_secs(5)).await;
    let st = feed.status();
    // Wire: handshake requested the named subscription with mirror-id.
    let frames = seen(&server);
    assert!(
        !frames.is_empty(),
        "server must observe the subscribe request"
    );
    let sub: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(sub["id"], json!(1));
    assert_eq!(sub["method"], "eth_subscribe");
    assert_eq!(sub["params"][0], SUB_METHOD);
    // Typed event parse.
    let ev = &evs[0];
    assert_eq!(ev.chain_id, 1);
    assert_eq!(
        ev.from.to_string(),
        "0x1111111111111111111111111111111111111111"
    );
    assert_eq!(
        ev.to.unwrap().to_string(),
        "0x2222222222222222222222222222222222222222"
    );
    assert_eq!(
        ev.value,
        json!("0x4fefa17b724000")
            .as_str()
            .map(|s| alloy::primitives::U256::from_str_radix(&s[2..], 16).unwrap())
            .unwrap()
    );
    assert_eq!(
        ev.data,
        alloy::primitives::Bytes::from(alloy::hex::decode("deadbeef").unwrap())
    );
    assert_eq!(ev.gas, 0x5208);
    assert_eq!(ev.max_fee_per_gas, 0x0007_e1c6_5b04);
    assert_eq!(ev.max_priority_fee_per_gas, 0);
    assert_eq!(ev.nonce, 0);
    assert_eq!(ev.tx_type, 2);
    assert_eq!(
        ev.hash.to_string(),
        "0x5f08dd372fce1a44dda27bed60ca036acb4979fad6ca37b9c388e351a870fe4c"
    );
    assert_eq!(st.accepted, 1);
    assert!(st.last_event_unix_ms > 0);
}

#[tokio::test]
async fn t02_non_mainnet_chain_id_event_is_rejected_and_counted() {
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Push(sample_event("0x89")),
        Step::Push(sample_event("0x1")),
    ]])
    .await;
    let feed = spawn_feed(cfg(server.addr.port()));
    let evs = drain_until(&feed, 1, Duration::from_secs(5)).await;
    assert_eq!(evs.len(), 1, "only the mainnet event drains");
    assert_eq!(evs[0].chain_id, 1);
    let st = feed.status();
    assert_eq!(st.rejected_chain_id, 1);
    assert_eq!(st.accepted, 1);
}

#[tokio::test]
async fn t03_server_drop_triggers_reconnect_and_resubscribe() {
    let h2 = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let server = start_server(vec![
        vec![
            Step::ExpectSubscribe,
            Step::Ack,
            Step::Push(sample_event("0x1")),
            Step::Close,
        ],
        vec![
            Step::ExpectSubscribe,
            Step::Ack,
            Step::Push(sample_tx("0x1", 1, h2)),
        ],
    ])
    .await;
    let feed = spawn_feed(cfg_with(server.addr.port(), Duration::from_secs(30), 64));
    let evs = drain_until(&feed, 2, Duration::from_secs(10)).await;
    assert_eq!(
        evs[0].hash.to_string(),
        "0x5f08dd372fce1a44dda27bed60ca036acb4979fad6ca37b9c388e351a870fe4c"
    );
    assert_eq!(evs[1].hash.to_string(), h2);
    let st = feed.status();
    assert!(st.reconnects >= 1, "reconnects recorded");
    // The server saw TWO subscribe handshakes.
    let frames = seen(&server);
    assert_eq!(frames.len(), 2, "resubscribe on reconnect");
}

#[tokio::test]
async fn t04_silent_socket_within_watchdog_is_torn_down_and_reconnected() {
    let server = start_server(vec![
        vec![Step::ExpectSubscribe, Step::Ack, Step::Close],
        vec![
            Step::ExpectSubscribe,
            Step::Ack,
            Step::Push(sample_event("0x1")),
            Step::Hold,
        ],
    ])
    .await;
    // 200ms watchdog: a silent socket is reaped quickly.
    let feed = spawn_feed(cfg_with(server.addr.port(), Duration::from_millis(200), 64));
    let evs = drain_until(&feed, 1, Duration::from_secs(10)).await;
    assert_eq!(evs.len(), 1);
    let st = feed.status();
    assert!(st.reconnects >= 1, "stall forces reconnect counter");
    assert_eq!(seen(&server).len(), 2, "rehandshake after stall");
}

#[tokio::test]
async fn t05_ring_capacity_evicts_oldest_and_counts_drops() {
    let mut steps = vec![Step::ExpectSubscribe, Step::Ack];
    for i in 0u64..12 {
        let h = format!("0x{i:064x}");
        steps.push(Step::Push(sample_tx("0x1", i, &h)));
    }
    let server = start_server(vec![steps]).await;
    let feed = spawn_feed(cfg_with(server.addr.port(), Duration::from_secs(30), 8));
    let evs = drain_until(&feed, 8, Duration::from_secs(5)).await;
    assert_eq!(evs.len(), 8);
    // Newest 8 (nonces 4..=11), in order.
    for (i, ev) in evs.iter().enumerate() {
        assert_eq!(ev.nonce, (i + 4) as u64);
    }
    let st = feed.status();
    assert_eq!(st.dropped_ring, 4, "12 pushed into ring 8 => 4 evicted");
    assert_eq!(st.accepted, 12);
}

// Live network probe: proves the client against the REAL MEVBlocker stream.
// Ignored by default (network-dependent); run explicitly:
//   cargo test -p degenbot-rpc --test backrun_feed -- --ignored live
#[tokio::test]
#[ignore = "live network: connects to wss://searchers.mevblocker.io"]
async fn live_stream_probe_delivers_real_events() {
    let feed = spawn_feed(BackrunFeedConfig {
        // Backrunnable flow arrives in bursts; wait patiently for one burst.
        watchdog: Duration::from_secs(30),
        ..BackrunFeedConfig::for_mainnet()
    });
    let _ = drain_until(&feed, 1, Duration::from_secs(120)).await;
    let st = feed.status();
    assert!(st.accepted >= 1, "live stream delivered no events: {st:?}");
    assert_eq!(st.rejected_chain_id, 0, "live stream must be mainnet");
    assert!(st.last_event_unix_ms > 0);
    feed.stop();
}
#[tokio::test]
async fn t06_malformed_notification_is_counted_not_fatal() {
    // Structurally-valid JSON frame, but the payload is a bare string.
    let push_non_object = json!("not an object");
    // Structurally valid notification, but result misses required fields (no
    // `from`, no chainId parse target).
    let push_missing_fields = json!({
        "jsonrpc": "2.0",
        "method": "eth_subscription",
        "params": {
            "subscription": "0xsubid",
            "result": {
                "to": "0x2222222222222222222222222222222222222222",
            },
        },
    });
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Push(push_non_object),
        Step::Push(push_missing_fields),
        Step::Push(sample_event("0x1")),
    ]])
    .await;
    let feed = spawn_feed(cfg(server.addr.port()));
    // Both malformed pushes are counted; the last valid event still drains,
    // proving the connection survived.
    let evs = drain_until(&feed, 1, Duration::from_secs(5)).await;
    assert_eq!(evs.len(), 1);
    let st = feed.status();
    assert_eq!(st.rejected_parse, 2);
    assert_eq!(st.accepted, 1);
}

#[tokio::test]
async fn t07_drain_returns_unbatched_frames_oldest_first() {
    // Characterization (Phase B1): with the ring under capacity, `drain()`
    // returns every frame in arrival order, oldest first, as one atomic take.
    let mut steps = vec![Step::ExpectSubscribe, Step::Ack];
    let mut hashes = Vec::new();
    for nonce in 0u64..5 {
        let hash = format!("0x{:064x}", 0xabc0u64 + nonce);
        hashes.push(hash.clone());
        steps.push(Step::Push(sample_tx("0x1", nonce, &hash)));
    }
    let server = start_server(vec![steps]).await;
    // `for_mainnet` capacity is 4096 and only five frames arrive: no eviction.
    let feed = spawn_feed(cfg(server.addr.port()));
    let evs = drain_until(&feed, 5, Duration::from_secs(5)).await;
    assert_eq!(evs.len(), 5, "unbatched frames are all retained");
    for (i, ev) in evs.iter().enumerate() {
        assert_eq!(ev.nonce, i as u64, "frames drain in arrival order");
        assert_eq!(ev.hash.to_string(), hashes[i], "oldest frame is first");
    }
    let st = feed.status();
    assert_eq!(st.dropped_ring, 0, "under capacity, nothing evicted");
    assert_eq!(st.accepted, 5);
    assert!(feed.drain().is_empty(), "drain is an atomic take");
}

// ── INFO lifecycle logging ─────────────────────────────────────────────
//
// The `op_*` events emit under the closed `degenbot::rpc` domain target, so
// the capture layer filters on it exactly like the provider's retry-loop
// capture (same once-per-process constraint: a `Once`-installed global
// subscriber forwards every matching event into a SHARED collector — events
// emit from the pump task's worker threads, so thread-local capture cannot
// see them).

/// Install (once) the capturing subscriber and return the shared collector.
/// Lines are `"{level}|{message}"`.
fn feed_log_capture() -> Arc<StdMutex<Vec<String>>> {
    static INSTALL: Once = Once::new();
    static COLLECTOR: OnceLock<Arc<StdMutex<Vec<String>>>> = OnceLock::new();
    let collector = COLLECTOR.get_or_init(|| Arc::new(StdMutex::new(Vec::new())));
    INSTALL.call_once(|| {
        use tracing_subscriber::layer::{Context, Layer};
        use tracing_subscriber::registry::LookupSpan;

        struct MessageCollector(String);
        impl tracing::field::Visit for MessageCollector {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    let s = format!("{v:?}");
                    let unquoted = s
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .unwrap_or(&s);
                    self.0.clear();
                    self.0.push_str(unquoted);
                }
            }

            fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                if f.name() == "message" {
                    self.0.clear();
                    self.0.push_str(v);
                }
            }
        }

        struct Capture(Arc<StdMutex<Vec<String>>>);
        impl<S> Layer<S> for Capture
        where
            S: tracing::Subscriber + for<'a> LookupSpan<'a>,
        {
            fn on_event(&self, event: &tracing::Event, _ctx: Context<'_, S>) {
                if event.metadata().target() != "degenbot::rpc" {
                    return;
                }
                let mut collector = MessageCollector(String::new());
                event.record(&mut collector);
                self.0.lock().unwrap().push(format!(
                    "{}|{}",
                    event.metadata().level(),
                    collector.0
                ));
            }
        }

        use tracing_subscriber::layer::SubscriberExt;
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(Capture(Arc::clone(collector))),
        );
    });
    Arc::clone(collector)
}

/// Bounded wait for a captured line matching `needle` (concurrent tests may
/// interleave captures, so presence — not order — is the assertion).
async fn wait_for_line(capture: &Arc<StdMutex<Vec<String>>>, needle: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        {
            let lines = capture.lock().unwrap();
            if lines.iter().any(|l| l.contains(needle)) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for captured line {needle:?}; have: {:?}",
            capture.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The feed's INFO lifecycle: connect → subscribed → per-payload →
/// disconnect — all at INFO level, all under the `degenbot::rpc` domain.
#[tokio::test]
async fn t08_info_logs_cover_connect_subscribed_payload_disconnect() {
    let capture = feed_log_capture();
    let h2 = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let server = start_server(vec![
        vec![
            Step::ExpectSubscribe,
            Step::Ack,
            Step::Push(sample_event("0x1")),
            Step::Close,
        ],
        vec![
            Step::ExpectSubscribe,
            Step::Ack,
            Step::Push(sample_tx("0x1", 1, h2)),
            Step::Hold,
        ],
    ])
    .await;
    let feed = spawn_feed(cfg_with(server.addr.port(), Duration::from_secs(30), 64));
    let _ = drain_until(&feed, 2, Duration::from_secs(10)).await;

    wait_for_line(&capture, "backrun feed: connected", 10).await;
    wait_for_line(&capture, "backrun feed: subscribed", 10).await;
    wait_for_line(&capture, "backrun feed: pending tx", 10).await;
    wait_for_line(&capture, "backrun feed: disconnected", 10).await;
    // All lifecycle lines are INFO (not the pre-existing WARN family).
    {
        let lines = capture.lock().unwrap();
        for needle in [
            "backrun feed: connected",
            "backrun feed: subscribed",
            "backrun feed: pending tx",
            "backrun feed: disconnected",
        ] {
            assert!(
                lines.iter().any(|l| l.contains(needle)),
                "missing captured line {needle:?}"
            );
            let hit = lines.iter().find(|l| l.contains(needle)).expect("checked");
            assert!(
                hit.starts_with("INFO|"),
                "{needle} must log at INFO, got: {hit}"
            );
        }
    }
    feed.stop();
}
