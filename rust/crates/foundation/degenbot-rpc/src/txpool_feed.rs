//! Node txpool pending-transaction feed.
//!
//! Connects a WebSocket to the CHAIN NODE, subscribes to
//! `newPendingTransactions`, and resolves each delivered hash with
//! `eth_getTransactionByHash` over the same socket to build the full pending
//! tx. Typed `HubEvent::PendingTx`s flow through a hub-owned bounded ring
//! (the `DropOldestCounted` policy) drained by [`TxpoolFeed::drain`] — the
//! same vocabulary the `MEVBlocker` feed publishes, so the frame pipeline is
//! feed-agnostic.
//!
//! Wire notes:
//! - subscribe: `{"jsonrpc":"2.0","id":1,"method":"eth_subscribe",
//!   "params":["newPendingTransactions"]}`; the ack mirrors `id`.
//! - hash notices: `eth_subscription` whose `params.result` is the tx hash
//!   (hash-only notice is the portable geth/erigon shape; a full-tx object
//!   result is also accepted).
//! - fetch: `eth_getTransactionByHash` with a self-incrementing request id;
//!   a `null` result means the tx mined before it could be fetched, which is
//!   counted and dropped (never fatal).
//!
//! Fee normalization: a post-London endpoint always prices via max-fee caps;
//! a pre-1559 (legacy or access-list) tx's `gasPrice` is the effective fee,
//! so it fills BOTH caps (the frame's EIP-1559 fee fields become the
//! effective ceiling).
//!
//! Robustness mirrors `backrun_feed`'s watchdog: a frame-silent socket is torn
//! down and reconnected; any server close reconnects with capped exponential
//! backoff that resets once a session proves it can deliver events. Non-1
//! chain ids and malformed payloads are counted, never fatal.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, Bytes, B256, U256};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value as Json;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use degenbot_core::{op_info, op_warn};
use degenbot_eventhub::{
    DropOldestReceiver, DropOldestSender, Hub, HubClass, HubError, HubEvent, OverflowPolicy,
    SourceHandle, Subscription,
};

/// The hub `DropOldestCounted` counter label this feed registers under; the
/// same string is exposed as `TxpoolFeedStatus::dropped_ring`.
pub const TXPOOL_DROPPED_RING_METRIC: &str = "dropped_ring";

const SUBSCRIBE_ID: u64 = 1;
const SUBSCRIBE_METHOD: &str = "newPendingTransactions";
/// First auto id for a hash fetch (the subscribe ack owns `SUBSCRIBE_ID`).
const FETCH_ID_BASE: u64 = 2;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct TxpoolFeedConfig {
    pub ws_url: String,
    pub expected_chain_id: u64,
    /// Tear down + reconnect when no frame (the subscribe ack included)
    /// arrives within the window.
    pub watchdog: Duration,
    pub ring_capacity: usize,
    pub reconnect_backoff: Duration,
    pub max_backoff: Duration,
}

impl TxpoolFeedConfig {
    /// Production default: the chain node WS (resolved per-ecosystem by the
    /// driver boot), 16s stall watchdog (a healthy node streams pending txs
    /// far more often than it produces heads), 4096-event ring, 250ms initial
    /// backoff capped at 5s, chain id 1 enforced.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            ws_url: String::new(),
            expected_chain_id: 1,
            watchdog: Duration::from_secs(16),
            ring_capacity: 4096,
            reconnect_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
        }
    }
}

/// Handle over a spawned txpool feed pump.
pub struct TxpoolFeed {
    shared: Arc<Shared>,
    ring: DropOldestReceiver,
    stop_tx: watch::Sender<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TxpoolFeedStatus {
    pub connected: bool,
    pub accepted: u64,
    pub dropped_ring: u64,
    pub rejected_chain_id: u64,
    pub rejected_parse: u64,
    /// The tx mined before its hash could be fetched (`null` by-hash result).
    pub mine_misses: u64,
    pub reconnects: u64,
    pub last_event_unix_ms: u64,
}

struct Shared {
    cfg_backoff: (Duration, Duration),
    source: DropOldestSender,
    connected: AtomicBool,
    accepted: AtomicU64,
    rejected_chain_id: AtomicU64,
    rejected_parse: AtomicU64,
    mine_misses: AtomicU64,
    reconnects: AtomicU64,
    last_event_unix_ms: AtomicU64,
}

impl Shared {
    fn push(&self, ev: degenbot_eventhub::PendingTx) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix_ms
            .store(now_unix_ms(), Ordering::Relaxed);
        self.source.push(HubEvent::PendingTx(ev));
    }

    fn count(&self, which: Count) {
        let c = match which {
            Count::RejectedChainId => &self.rejected_chain_id,
            Count::RejectedParse => &self.rejected_parse,
            Count::MineMisses => &self.mine_misses,
            Count::Reconnects => &self.reconnects,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
enum Count {
    RejectedChainId,
    RejectedParse,
    MineMisses,
    Reconnects,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

impl TxpoolFeed {
    /// Spawn the pump on a private, unattached drop-oldest ring (the
    /// Python-exposed single-feed path). Must be called within a tokio
    /// runtime context, or the crate-global runtime is used.
    #[must_use]
    pub fn spawn(cfg: TxpoolFeedConfig) -> Self {
        let hub = Hub::new();
        let (source, ring) =
            hub.detached_drop_oldest(TXPOOL_DROPPED_RING_METRIC, cfg.ring_capacity);
        Self::build(cfg, source, ring)
    }

    /// Spawn the pump registering its `PendingTx` drop-oldest ring on `hub`.
    ///
    /// # Errors
    ///
    /// Propagates `HubError` when the hub already holds a `PendingTx` source
    /// or the policy handle mismatches.
    pub fn spawn_on_hub(hub: &Hub, cfg: TxpoolFeedConfig) -> Result<Self, HubError> {
        let SourceHandle::DropOldestCounted(source) = hub.register_source(
            HubClass::PendingTx,
            OverflowPolicy::DropOldestCounted {
                name: TXPOOL_DROPPED_RING_METRIC,
            },
            cfg.ring_capacity,
        )?
        else {
            return Err(HubError::PolicyMismatch {
                expected: "DropOldestCounted",
            });
        };
        let Subscription::DropOldestCounted(ring) = hub.subscribe(HubClass::PendingTx)? else {
            return Err(HubError::PolicyMismatch {
                expected: "DropOldestCounted",
            });
        };
        Ok(Self::build(cfg, source, ring))
    }

    fn build(cfg: TxpoolFeedConfig, source: DropOldestSender, ring: DropOldestReceiver) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            cfg_backoff: (cfg.reconnect_backoff, cfg.max_backoff),
            source,
            connected: AtomicBool::new(false),
            accepted: AtomicU64::new(0),
            rejected_chain_id: AtomicU64::new(0),
            rejected_parse: AtomicU64::new(0),
            mine_misses: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            last_event_unix_ms: AtomicU64::new(0),
        });
        let sh = shared.clone();
        let fut = async move { run_loop(cfg, sh, stop_rx).await };
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(fut);
        } else {
            degenbot_core::runtime::get_runtime().spawn(fut);
        }
        Self {
            shared,
            ring,
            stop_tx,
        }
    }

    /// All buffered events, ordered, atomically.
    #[must_use]
    pub fn drain(&self) -> Vec<degenbot_eventhub::PendingTx> {
        self.ring
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                HubEvent::PendingTx(tx) => Some(tx),
                other => {
                    op_warn!(
                        domain = rpc,
                        event = ?other,
                        "txpool feed: non-pending event on the pending ring"
                    );
                    None
                }
            })
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> TxpoolFeedStatus {
        let s = &self.shared;
        TxpoolFeedStatus {
            connected: s.connected.load(Ordering::Relaxed),
            accepted: s.accepted.load(Ordering::Relaxed),
            dropped_ring: self.ring.dropped(),
            rejected_chain_id: s.rejected_chain_id.load(Ordering::Relaxed),
            rejected_parse: s.rejected_parse.load(Ordering::Relaxed),
            mine_misses: s.mine_misses.load(Ordering::Relaxed),
            reconnects: s.reconnects.load(Ordering::Relaxed),
            last_event_unix_ms: s.last_event_unix_ms.load(Ordering::Relaxed),
        }
    }

    /// Politely stop the pump (idempotent).
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for TxpoolFeed {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SessionEnd {
    Stopped,
    Closed,
    Stall,
}

async fn run_loop(cfg: TxpoolFeedConfig, s: Arc<Shared>, mut stop_rx: watch::Receiver<bool>) {
    let (init_backoff, max_backoff) = s.cfg_backoff;
    let mut backoff = init_backoff;
    loop {
        if *stop_rx.borrow() {
            return;
        }
        s.connected.store(false, Ordering::Relaxed);
        let request = match cfg.ws_url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                op_warn!(domain = rpc, url = %cfg.ws_url, e = %e, "txpool feed: invalid url");
                return;
            }
        };
        let socket = match tokio::time::timeout(cfg.watchdog, connect_async(request)).await {
            Ok(Ok((ws, _resp))) => {
                op_info!(domain = rpc, url = %cfg.ws_url, "txpool feed: connected");
                ws
            }
            Ok(Err(e)) => {
                s.count(Count::Reconnects);
                op_warn!(domain = rpc, %e, "txpool feed: connect failed");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
                continue;
            }
            Err(_) => {
                s.count(Count::Reconnects);
                op_warn!(domain = rpc, "txpool feed: connect timeout");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
                continue;
            }
        };
        let accepted_before = s.accepted.load(Ordering::Relaxed);
        match session(socket, &cfg, &s, &mut stop_rx).await {
            SessionEnd::Stopped => {
                op_info!(domain = rpc, url = %cfg.ws_url, "txpool feed: stopped");
                return;
            }
            end @ (SessionEnd::Closed | SessionEnd::Stall) => {
                let session_accepted = s.accepted.load(Ordering::Relaxed) - accepted_before;
                if session_accepted > 0 {
                    backoff = init_backoff;
                }
                s.connected.store(false, Ordering::Relaxed);
                s.count(Count::Reconnects);
                op_info!(
                    domain = rpc,
                    url = %cfg.ws_url,
                    reason = if end == SessionEnd::Stall { "stall" } else { "closed" },
                    session_accepted,
                    reconnects = s.reconnects.load(Ordering::Relaxed),
                    "txpool feed: disconnected"
                );
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
            }
        }
    }
}

#[expect(clippy::too_many_lines, reason = "the ws session reads top-to-bottom")]
async fn session(
    mut ws: WsStream,
    cfg: &TxpoolFeedConfig,
    s: &Shared,
    stop_rx: &mut watch::Receiver<bool>,
) -> SessionEnd {
    let sub_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": SUBSCRIBE_ID,
        "method": "eth_subscribe",
        "params": [SUBSCRIBE_METHOD]
    });
    if let Err(e) = ws.send(Message::Text(sub_req.to_string().into())).await {
        op_warn!(domain = rpc, %e, "txpool feed: subscribe send failed");
        return SessionEnd::Closed;
    }
    let mut sub_id: Option<String> = None;
    // in-flight hash fetches: request id -> tx hash.
    let mut fetches: HashMap<u64, B256> = HashMap::new();
    let mut next_fetch_id = FETCH_ID_BASE;
    s.connected.store(true, Ordering::Relaxed);
    loop {
        let frame = tokio::select! {
            () = tokio::time::sleep(cfg.watchdog) => return SessionEnd::Stall,
            Ok(()) = stop_rx.changed() => {
                let _ = ws.send(Message::Close(None)).await;
                return SessionEnd::Stopped;
            }
            f = ws.next() => match f {
                None => return SessionEnd::Closed,
                Some(Err(e)) => {
                    op_warn!(domain = rpc, %e, "txpool feed: stream error");
                    return SessionEnd::Closed;
                }
                Some(Ok(m)) => m,
            },
        };
        let Message::Text(t) = frame else {
            continue;
        };
        let Ok(v): Result<Json, _> = serde_json::from_str(t.as_str()) else {
            s.count(Count::RejectedParse);
            continue;
        };
        // Subscribe ack: mirror id, string result, no `method` field.
        if v.get("method").is_none() && v.get("id").and_then(Json::as_u64) == Some(SUBSCRIBE_ID) {
            if let Some(id) = v.get("result").and_then(Json::as_str) {
                sub_id = Some(id.to_string());
                op_info!(domain = rpc, subscription = %id, "txpool feed: subscribed");
            }
            continue;
        }
        // By-hash fetch response: the tx object (or null = mined first).
        if v.get("method").is_none() {
            let resolved = v
                .get("id")
                .and_then(Json::as_u64)
                .and_then(|id| fetches.remove(&id).map(|hash| (id, hash)));
            if let Some((req_id, hash)) = resolved {
                let _ = req_id;
                match v.get("result") {
                    Some(Json::Null) | None => {
                        s.count(Count::MineMisses);
                    }
                    Some(result) => {
                        if let Some(ev) = parse_full_tx(result, hash, cfg.expected_chain_id, s) {
                            op_info!(
                                domain = rpc,
                                hash = %ev.hash,
                                from = %ev.from,
                                to = ?ev.to,
                                chain_id = ev.chain_id,
                                gas = ev.gas,
                                nonce = ev.nonce,
                                "txpool feed: pending tx"
                            );
                            s.push(ev);
                        }
                    }
                }
                continue;
            }
            s.count(Count::RejectedParse);
            continue;
        }
        // Subscription notice: the hash (or, on nodes that send full bodies,
        // the tx object itself).)
        let is_sub_notif = v.get("method").and_then(Json::as_str) == Some("eth_subscription");
        let sub_matches = sub_id
            .as_deref()
            .zip(v.pointer("/params/subscription").and_then(Json::as_str))
            .is_some_and(|(a, b)| a == b);
        if !(is_sub_notif && sub_matches) {
            s.count(Count::RejectedParse);
            continue;
        }
        let Some(result) = v.pointer("/params/result") else {
            s.count(Count::RejectedParse);
            continue;
        };
        match result {
            Json::String(hash_str) => {
                let Ok(hash) = hash_str
                    .strip_prefix("0x")
                    .unwrap_or(hash_str)
                    .parse::<B256>()
                else {
                    s.count(Count::RejectedParse);
                    continue;
                };
                let id = next_fetch_id;
                next_fetch_id = next_fetch_id.wrapping_add(1).max(FETCH_ID_BASE);
                fetches.insert(id, hash);
                let fetch_req = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "eth_getTransactionByHash",
                    "params": [hash]
                });
                if let Err(e) = ws.send(Message::Text(fetch_req.to_string().into())).await {
                    op_warn!(domain = rpc, %e, "txpool feed: fetch send failed");
                    return SessionEnd::Closed;
                }
            }
            // A node delivering full bodies straight on the sub: parse it.
            Json::Object(_) => {
                let Some(hash) = result.get("hash").and_then(|h| {
                    h.as_str()
                        .and_then(|s| s.strip_prefix("0x").unwrap_or(s).parse::<B256>().ok())
                }) else {
                    s.count(Count::RejectedParse);
                    continue;
                };
                if let Some(ev) = parse_full_tx(result, hash, cfg.expected_chain_id, s) {
                    s.push(ev);
                }
            }
            _ => s.count(Count::RejectedParse),
        }
    }
}

fn parse_full_tx(
    v: &Json,
    hash: B256,
    expected_chain_id: u64,
    s: &Shared,
) -> Option<degenbot_eventhub::PendingTx> {
    let Some(from) = str_field(v, "from").and_then(|x| x.parse::<Address>().ok()) else {
        s.count(Count::RejectedParse);
        return None;
    };
    let chain_id = hex_u64(v.get("chainId")).unwrap_or_default();
    if chain_id != expected_chain_id {
        s.count(Count::RejectedChainId);
        return None;
    }
    let data = v
        .get("input")
        .or_else(|| v.get("data"))
        .and_then(Json::as_str)
        .and_then(|raw| alloy::hex::decode(raw.trim_start_matches("0x")).ok())
        .map(Bytes::from)
        .unwrap_or_default();
    let raw = v
        .get("raw")
        .and_then(Json::as_str)
        .and_then(|x| alloy::hex::decode(x.trim_start_matches("0x")).ok())
        .map(Bytes::from);
    // Fee normalization: resource pricing collapses to the EIP-1559 caps.
    // 1559 -> direct; legacy/access-list -> the gas price IS the effective
    // fee, so it fills both caps.
    let max_fee = hex_u128(v.get("maxFeePerGas"));
    let max_priority = hex_u128(v.get("maxPriorityFeePerGas"));
    let (max_fee, max_priority) = if let (Some(f), Some(p)) = (max_fee, max_priority) {
        (f, p)
    } else {
        let Some(price) = hex_u128(v.get("gasPrice")) else {
            s.count(Count::RejectedParse);
            return None;
        };
        (price, price)
    };
    Some(degenbot_eventhub::PendingTx {
        chain_id,
        from,
        to: str_field(v, "to").and_then(|x| x.parse::<Address>().ok()),
        value: v
            .get("value")
            .and_then(Json::as_str)
            .map(hex_u256)
            .unwrap_or_default()
            .unwrap_or_default(),
        data,
        gas: hex_u64(v.get("gas")).unwrap_or_default(),
        max_fee_per_gas: max_fee,
        max_priority_fee_per_gas: max_priority,
        nonce: hex_u64(v.get("nonce")).unwrap_or_default(),
        hash,
        access_list: v.get("accessList").cloned().unwrap_or(Json::Null),
        tx_type: hex_u64(v.get("type"))
            .and_then(|t| u8::try_from(t).ok())
            .unwrap_or_default(),
        received_unix_ms: now_unix_ms(),
        raw_signed_tx: raw,
    })
}

fn str_field<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Json::as_str)
}

fn hex_u64(v: Option<&Json>) -> Option<u64> {
    let s = v?.as_str()?;
    u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

fn hex_u128(v: Option<&Json>) -> Option<u128> {
    let s = v?.as_str()?;
    u128::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

fn hex_u256(s: &str) -> Option<U256> {
    U256::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}
