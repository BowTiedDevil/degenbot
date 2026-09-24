//! Txpool feed client spec.
//!
//! Seam: `degenbot_rpc::txpool_feed::{TxpoolFeed, TxpoolFeedConfig, TxpoolFeedStatus}`
//! and the hub `PendingTx` events it emits. Tests drive a local mock WS server
//! and assert on wire behavior: the subscribe handshake, hash-fetch correlation,
//! full-tx parse (1559 + legacy fee mapping + raw bytes), chain-id gate, and
//! mine-miss counting.

#![expect(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use degenbot_rpc::txpool_feed::{TxpoolFeed, TxpoolFeedConfig};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value as Json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{accept_async, WebSocketStream};

const TX_HASH_FULL: &str = "0x5f08dd372fce1a44dda27bed60ca036acb4979fad6ca37b9c388e351a870fe4c";
const TX_HASH_LEGACY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

enum Step {
    /// Await + assert the subscribe handshake.
    ExpectSubscribe,
    Ack,
    /// Push an `eth_subscription` notice carrying the pending tx hash.
    Notice(String),
    /// Reply to the next fetch request (id stamped from the observed request)
    /// with the canned result.
    FetchResult(Json),
}

fn full_tx_1559() -> Json {
    json!({
        "result": {
            "blockHash": null,
            "chainId": "0x1",
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "value": "0x1a",
            "input": "0xdeadbeef",
            "gas": "0x5208",
            "maxFeePerGas": "0x7e1c65b04",
            "maxPriorityFeePerGas": "0x3b9aca00",
            "nonce": "0x2a",
            "hash": TX_HASH_FULL,
            "type": "0x2",
            "accessList": [{ "address": "0x3333333333333333333333333333333333333333", "storageKeys": [] }],
            "raw": "0xf86c0184"
        }
    })
}

fn full_tx_legacy() -> Json {
    json!({
        "result": {
            "chainId": "0x1",
            "from": "0x4444444444444444444444444444444444444444",
            "to": "0x5555555555555555555555555555555555555555",
            "value": "0x0",
            "input": "0x",
            "gas": "0x186a0",
            "gasPrice": "0x77359400",
            "nonce": "0x7",
            "hash": TX_HASH_LEGACY,
            "type": "0x0"
        }
    })
}

fn notice_step(hash: &str) -> Json {
    json!({
        "jsonrpc": "2.0",
        "method": "eth_subscription",
        "params": { "subscription": "0xsubid", "result": hash }
    })
}

impl Step {
    async fn serve(self, ws: &mut WebSocketStream<tokio::net::TcpStream>, state: &mut Vec<String>) {
        match self {
            Step::ExpectSubscribe => {
                match tokio::time::timeout(Duration::from_secs(30), ws.next()).await {
                    Ok(Some(Ok(WsMessage::Text(t)))) => assert!(
                        t.contains("eth_subscribe") && t.contains("newPendingTransactions"),
                        "handshake must request newPendingTransactions, got: {t}"
                    ),
                    other => panic!("expected subscribe frame, got {other:?}"),
                }
            }
            Step::Ack => {
                ws.send(WsMessage::Text(
                    r#"{"jsonrpc":"2.0","id":1,"result":"0xsubid"}"#.into(),
                ))
                .await
                .unwrap();
            }
            Step::Notice(hash) => {
                ws.send(WsMessage::Text(notice_step(&hash).to_string().into()))
                    .await
                    .unwrap();
            }
            Step::FetchResult(result) => {
                // South-abound: await the feed's eth_getTransactionByHash
                // request, stamp the canned result with its request id, reply.
                let t = loop {
                    let frame = tokio::time::timeout(Duration::from_secs(30), ws.next())
                        .await
                        .expect("fetch request arrives")
                        .expect("fetch stream open")
                        .expect("fetch frame decodes");
                    let WsMessage::Text(t) = frame else {
                        continue;
                    };
                    if t.contains("eth_getTransactionByHash") {
                        break t;
                    }
                };
                state.push(t.to_string());
                let req: Json = serde_json::from_str(&t).unwrap();
                let id = req.get("id").and_then(Json::as_u64).unwrap();
                let mut reply = json!({"jsonrpc": "2.0", "id": id});
                reply["result"] = result.get("result").cloned().unwrap_or(Json::Null);
                ws.send(WsMessage::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        }
    }
}

async fn serve_conn(mut ws: WebSocketStream<tokio::net::TcpStream>, steps: Vec<Step>) {
    let mut seen: Vec<String> = Vec::new();
    for step in steps {
        step.serve(&mut ws, &mut seen).await;
    }
    // South.YELLOW: hold the socket open so the feed keeps its session while
    // assertions run on a paused-clock-free timeline.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;
}

struct MockServer {
    addr: SocketAddr,
}

async fn start_server(scripts: Vec<Vec<Step>>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut scripts = scripts.into_iter();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Some(script) = scripts.next() else {
                continue;
            };
            tokio::spawn(async move {
                if let Ok(ws) = accept_async(stream).await {
                    serve_conn(ws, script).await;
                }
            });
        }
    });
    MockServer { addr }
}

fn cfg(port: u16) -> TxpoolFeedConfig {
    TxpoolFeedConfig {
        ws_url: format!("ws://127.0.0.1:{port}"),
        // Generous under a full parallel test-suite run: the watchdog is a
        // stall detector, and CI contention must not tear down a live
        // session that is merely slow to be scheduled.
        watchdog: Duration::from_secs(30),
        ring_capacity: 32,
        reconnect_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(50),
        ..TxpoolFeedConfig::defaults()
    }
}

async fn drain_until(feed: &TxpoolFeed, want: usize, timeout: Duration) -> Vec<Json> {
    let mut acc: Vec<Json> = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        for ev in feed.drain() {
            acc.push(json!({
                "hash": ev.hash.to_string(),
                "from": ev.from.to_string(),
                "to": ev.to.map(|a| a.to_string()),
                "value": format!("0x{:x}", ev.value),
                "data": format!("0x{}", alloy::hex::encode(&ev.data)),
                "gas": ev.gas,
                "maxFeePerGas": ev.max_fee_per_gas,
                "maxPriorityFeePerGas": ev.max_priority_fee_per_gas,
                "nonce": ev.nonce,
                "chainId": ev.chain_id,
                "tx_type": ev.tx_type,
                "raw": ev.raw_signed_tx.map(|b| format!("0x{}", alloy::hex::encode(&b))),
            }));
        }
        if acc.len() >= want {
            return acc;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want} events (got {}) status={:?}",
            acc.len(),
            feed.status()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Happy path: hash notice -> by-hash fetch -> full 1559 tx parsed with raw
/// bytes and direct fee caps.
#[tokio::test]
async fn hash_notice_fetches_and_publishes_full_tx() {
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Notice(TX_HASH_FULL.to_string()),
        Step::FetchResult(full_tx_1559()),
    ]])
    .await;
    let feed = TxpoolFeed::spawn(cfg(server.addr.port()));
    let events = drain_until(&feed, 1, Duration::from_secs(30)).await;
    feed.stop();

    assert_eq!(events[0]["hash"], TX_HASH_FULL);
    assert_eq!(
        events[0]["from"],
        "0x1111111111111111111111111111111111111111"
    );
    assert_eq!(
        events[0]["to"],
        "0x2222222222222222222222222222222222222222"
    );
    assert_eq!(events[0]["value"], "0x1a");
    assert_eq!(events[0]["data"], "0xdeadbeef");
    assert_eq!(events[0]["gas"], 0x5208);
    assert_eq!(events[0]["maxFeePerGas"], 0x0007_e1c6_5b04_u64);
    assert_eq!(events[0]["maxPriorityFeePerGas"], 0x3b9a_ca00);
    assert_eq!(events[0]["nonce"], 42);
    assert_eq!(events[0]["chainId"], 1);
    assert_eq!(events[0]["raw"], "0xf86c0184");
}

/// Legacy tx: the gas price IS the effective fee - both caps inherit it.
#[tokio::test]
async fn legacy_tx_maps_gas_price_to_both_caps() {
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Notice(TX_HASH_LEGACY.to_string()),
        Step::FetchResult(full_tx_legacy()),
    ]])
    .await;
    let feed = TxpoolFeed::spawn(cfg(server.addr.port()));
    let events = drain_until(&feed, 1, Duration::from_secs(30)).await;
    feed.stop();

    assert_eq!(events[0]["hash"], TX_HASH_LEGACY);
    assert_eq!(events[0]["tx_type"], 0);
    assert_eq!(events[0]["maxFeePerGas"], 0x7735_9400);
    assert_eq!(events[0]["maxPriorityFeePerGas"], 0x7735_9400);
    assert_eq!(events[0]["raw"], Json::Null);
}

/// A wrong-chain tx is rejected at parse and never published; a null fetch
/// result counts as a mine miss and the session keeps running.
#[tokio::test]
async fn chain_gate_and_null_fetch_are_counted_not_fatal() {
    let mut other = full_tx_1559();
    other["result"]["chainId"] = json!("0x89");
    let server = start_server(vec![vec![
        Step::ExpectSubscribe,
        Step::Ack,
        Step::Notice(TX_HASH_FULL.to_string()),
        Step::FetchResult(other),
        Step::Notice(TX_HASH_LEGACY.to_string()),
        Step::FetchResult(json!({ "result": null })),
    ]])
    .await;
    let feed = TxpoolFeed::spawn(cfg(server.addr.port()));
    // Let the pump consume both fetches; nothing may publish.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = feed.status();
    feed.stop();

    assert_eq!(feed.drain().len(), 0);
    assert!(
        status.rejected_chain_id >= 1,
        "wrong chain id must be counted: {status:?}"
    );
    assert!(
        status.mine_misses >= 1,
        "null fetch = mine miss: {status:?}"
    );
    assert!(
        status.connected || status.reconnects >= 1,
        "session survives rejected fetches: {status:?}"
    );
}
