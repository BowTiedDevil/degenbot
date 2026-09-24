//! `MEVBlocker` searcher feed client.
//!
//! Connects to `<wss://searchers.mevblocker.io>`, subscribes to
//! `mevblocker_partialPendingTransactions`, and delivers typed
//! [`BackrunFeedEvent`]s through a hub-owned bounded ring (the hub's
//! `DropOldestCounted` policy) drained by [`BackrunFeed::drain`]; the
//! `dropped_ring` counter is the hub channel's eviction count.
//!
//! Wire behavior (docs.mevblocker.io, searcher onboarding):
//! - subscribe: `{"jsonrpc":"2.0","id":1,"method":"eth_subscribe",
//!   "params":["mevblocker_partialPendingTransactions"]}`; the ack mirrors `id`.
//! - notifications: `eth_subscription` whose `params.subscription` matches the
//!   ack id; `params.result` is the unsigned pending tx (missing v/r/s) with
//!   chainId/to/value/data/accessList/nonce/fees/gas/type/hash/from.
//!
//! Robustness mirrors the provider crate's `pump_header_stream` watchdog: a
//! frame-silent socket is torn down and reconnected, any server close
//! reconnects with capped exponential backoff, and backoff resets once a
//! session proves it can deliver events. Non-1 chain ids and malformed
//! notifications are counted, never fatal.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::hex::FromHex;
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

/// The default searcher WS; re-exported so the readiness resolution and
/// this crate share one constant.
pub use degenbot_config::DEFAULT_BACKRUN_STREAM_URL as DEFAULT_STREAM_URL;
pub const BACKRUN_SUBSCRIPTION_METHOD: &str = "mevblocker_partialPendingTransactions";
/// The hub `DropOldestCounted` counter label the feed registers under; the
/// same string is exposed as [`BackrunFeedStatus::dropped_ring`].
pub const DROPPED_RING_METRIC: &str = "dropped_ring";

const SUBSCRIBE_ID: u64 = 1;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct BackrunFeedConfig {
    pub url: String,
    pub expected_chain_id: u64,
    /// Tear down + reconnect when no frame (including the subscribe ack, which
    /// this budget also covers) arrives within the window. Mirrors the header
    /// watchdog rationale in `subscription.rs`.
    pub watchdog: Duration,
    pub ring_capacity: usize,
    pub reconnect_backoff: Duration,
    pub max_backoff: Duration,
}

impl BackrunFeedConfig {
    /// Production default: mainnet `MEVBlocker` stream, 48s stall watchdog
    /// (~4x the 12s block time, matching the provider crate's header watchdog),
    /// 4096-event ring, 250ms initial backoff capped at 5s, chain id 1 enforced.
    #[must_use]
    pub fn for_mainnet() -> Self {
        Self {
            url: DEFAULT_STREAM_URL.to_string(),
            expected_chain_id: 1,
            watchdog: Duration::from_secs(48),
            ring_capacity: 4096,
            reconnect_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
        }
    }
}

/// One unsigned pending tx revealed by the `MEVBlocker` auction.
///
/// The hub's [`PendingTx`](degenbot_eventhub::PendingTx) vocabulary, re-exported
/// under the feed's historical name so consumers (the driver's frame
/// signature included) are unchanged by the ring's move onto the hub.
pub use degenbot_eventhub::PendingTx as BackrunFeedEvent;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackrunFeedStatus {
    pub connected: bool,
    pub accepted: u64,
    pub dropped_ring: u64,
    pub rejected_chain_id: u64,
    pub rejected_parse: u64,
    pub reconnects: u64,
    pub last_event_unix_ms: u64,
}

struct Shared {
    cfg_backoff: (Duration, Duration),
    /// The hub-owned drop-oldest ring's source side (capacity + eviction
    /// counter live on the hub channel, not here).
    source: DropOldestSender,
    connected: AtomicBool,
    accepted: AtomicU64,
    rejected_chain_id: AtomicU64,
    rejected_parse: AtomicU64,
    reconnects: AtomicU64,
    last_event_unix_ms: AtomicU64,
}

impl Shared {
    fn push(&self, ev: BackrunFeedEvent) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix_ms
            .store(now_unix_ms(), Ordering::Relaxed);
        self.source.push(HubEvent::PendingTx(ev));
    }

    fn count(&self, which: Count) {
        let c = match which {
            Count::RejectedChainId => &self.rejected_chain_id,
            Count::RejectedParse => &self.rejected_parse,
            Count::Reconnects => &self.reconnects,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
enum Count {
    RejectedChainId,
    RejectedParse,
    Reconnects,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Handle over a spawned feed pump.
pub struct BackrunFeed {
    shared: Arc<Shared>,
    /// The hub-owned ring's consumer side.
    ring: DropOldestReceiver,
    stop_tx: watch::Sender<bool>,
}

impl BackrunFeed {
    /// Spawn the pump on a private, unattached drop-oldest ring.
    ///
    /// This is the Python-exposed single-feed path
    /// (`degenbot._ffi.backrun`): the feed is its own host, so there is no
    /// shared hub registry to join. A process host that owns a [`Hub`] uses
    /// [`BackrunFeed::spawn_on_hub`] so the ring is reachable by subscribers.
    /// Must be called within a tokio runtime context, or the crate-global
    /// `degenbot_core::runtime::get_runtime()` is used.
    #[must_use]
    pub fn spawn(cfg: BackrunFeedConfig) -> Self {
        let hub = Hub::new();
        let (source, ring) = hub.detached_drop_oldest(DROPPED_RING_METRIC, cfg.ring_capacity);
        Self::build(cfg, source, ring)
    }

    /// Spawn the pump registering its `PendingTx` drop-oldest ring on `hub`.
    ///
    /// Registers the `PendingTx` class with `DropOldestCounted` (capacity
    /// `cfg.ring_capacity`, counter [`DROPPED_RING_METRIC`]) and subscribes its
    /// own consumer end. Must be called within a tokio runtime context, or the
    /// crate-global `degenbot_core::runtime::get_runtime()` is used.
    ///
    /// # Errors
    ///
    /// Propagates [`HubError`] if the hub already holds a `PendingTx` source
    /// or the declared policy somehow does not yield a drop-oldest handle.
    pub fn spawn_on_hub(hub: &Hub, cfg: BackrunFeedConfig) -> Result<Self, HubError> {
        let SourceHandle::DropOldestCounted(source) = hub.register_source(
            HubClass::PendingTx,
            OverflowPolicy::DropOldestCounted {
                name: DROPPED_RING_METRIC,
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

    /// Shared pump construction over a ring's two ends.
    fn build(cfg: BackrunFeedConfig, source: DropOldestSender, ring: DropOldestReceiver) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            cfg_backoff: (cfg.reconnect_backoff, cfg.max_backoff),
            source,
            connected: AtomicBool::new(false),
            accepted: AtomicU64::new(0),
            rejected_chain_id: AtomicU64::new(0),
            rejected_parse: AtomicU64::new(0),
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

    /// All buffered events, ordered, atomically (rings swap; concurrent drains
    /// may interleave but never duplicate or lose between both buffers).
    ///
    /// The hub ring only ever carries [`HubEvent::PendingTx`]; a stray other
    /// variant is warned about (never silently discarded).
    #[must_use]
    pub fn drain(&self) -> Vec<BackrunFeedEvent> {
        self.ring
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                HubEvent::PendingTx(tx) => Some(tx),
                other => {
                    op_warn!(
                        domain = rpc,
                        event = ?other,
                        "backrun feed: non-pending event on the pending ring"
                    );
                    None
                }
            })
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        let s = &self.shared;
        BackrunFeedStatus {
            connected: s.connected.load(Ordering::Relaxed),
            accepted: s.accepted.load(Ordering::Relaxed),
            dropped_ring: self.ring.dropped(),
            rejected_chain_id: s.rejected_chain_id.load(Ordering::Relaxed),
            rejected_parse: s.rejected_parse.load(Ordering::Relaxed),
            reconnects: s.reconnects.load(Ordering::Relaxed),
            last_event_unix_ms: s.last_event_unix_ms.load(Ordering::Relaxed),
        }
    }

    /// Politely stop the pump (idempotent).
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for BackrunFeed {
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

async fn run_loop(cfg: BackrunFeedConfig, s: Arc<Shared>, mut stop_rx: watch::Receiver<bool>) {
    let (init_backoff, max_backoff) = s.cfg_backoff;
    let mut backoff = init_backoff;
    loop {
        if *stop_rx.borrow() {
            return;
        }
        s.connected.store(false, Ordering::Relaxed);
        let request = match cfg.url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                op_warn!(domain = rpc, url = %cfg.url, e = %e, "backrun feed: invalid url");
                return;
            }
        };
        let socket = match tokio::time::timeout(cfg.watchdog, connect_async(request)).await {
            Ok(Ok((ws, _resp))) => {
                op_info!(domain = rpc, url = %cfg.url, "backrun feed: connected");
                ws
            }
            Ok(Err(e)) => {
                s.count(Count::Reconnects);
                op_warn!(domain = rpc, %e, "backrun feed: connect failed");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
                continue;
            }
            Err(_) => {
                s.count(Count::Reconnects);
                op_warn!(domain = rpc, "backrun feed: connect timeout");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
                continue;
            }
        };
        let accepted_before = s.accepted.load(Ordering::Relaxed);
        match session(socket, &cfg, &s, &mut stop_rx).await {
            SessionEnd::Stopped => {
                op_info!(domain = rpc, url = %cfg.url, "backrun feed: stopped");
                return;
            }
            end @ (SessionEnd::Closed | SessionEnd::Stall) => {
                let session_accepted = s.accepted.load(Ordering::Relaxed) - accepted_before;
                // A session that delivered events proved the transport; reset backoff.
                if session_accepted > 0 {
                    backoff = init_backoff;
                }
                s.connected.store(false, Ordering::Relaxed);
                s.count(Count::Reconnects);
                op_info!(
                    domain = rpc,
                    url = %cfg.url,
                    reason = if end == SessionEnd::Stall { "stall" } else { "closed" },
                    session_accepted,
                    reconnects = s.reconnects.load(Ordering::Relaxed),
                    "backrun feed: disconnected"
                );
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
            }
        }
    }
}

async fn session(
    mut ws: WsStream,
    cfg: &BackrunFeedConfig,
    s: &Shared,
    stop_rx: &mut watch::Receiver<bool>,
) -> SessionEnd {
    let sub_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": SUBSCRIBE_ID,
        "method": "eth_subscribe",
        "params": [BACKRUN_SUBSCRIPTION_METHOD]
    });
    if let Err(e) = ws.send(Message::Text(sub_req.to_string().into())).await {
        op_warn!(domain = rpc, %e, "backrun feed: subscribe send failed");
        return SessionEnd::Closed;
    }
    let mut sub_id: Option<String> = None;
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
                    op_warn!(domain = rpc, %e, "backrun feed: stream error");
                    return SessionEnd::Closed;
                }
                Some(Ok(m)) => m,
            },
        };
        match frame {
            Message::Text(t) => {
                let Ok(v): Result<Json, _> = serde_json::from_str(t.as_str()) else {
                    s.count(Count::RejectedParse);
                    continue;
                };
                // Subscribe ack: mirror id, string result, no `method` field.
                if v.get("method").is_none()
                    && v.get("id").and_then(Json::as_u64) == Some(SUBSCRIBE_ID)
                {
                    if let Some(id) = v.get("result").and_then(Json::as_str) {
                        sub_id = Some(id.to_string());
                        op_info!(domain = rpc, subscription = %id, "backrun feed: subscribed");
                    }
                    continue;
                }
                let is_sub_notif =
                    v.get("method").and_then(Json::as_str) == Some("eth_subscription");
                let sub_matches = sub_id
                    .as_deref()
                    .zip(v.pointer("/params/subscription").and_then(Json::as_str))
                    .is_some_and(|(a, b)| a == b);
                if is_sub_notif && sub_matches {
                    match v.pointer("/params/result") {
                        Some(result) => {
                            if let Some(ev) = parse_event(result, cfg.expected_chain_id, s) {
                                op_info!(
                                    domain = rpc,
                                    hash = %ev.hash,
                                    from = %ev.from,
                                    to = ?ev.to,
                                    chain_id = ev.chain_id,
                                    gas = ev.gas,
                                    nonce = ev.nonce,
                                    "backrun feed: pending tx"
                                );
                                s.push(ev);
                            }
                        }
                        None => s.count(Count::RejectedParse),
                    }
                    continue;
                }
                // Server-initiated tx stream frames we do not recognize (e.g.
                // a different subscription id) count as parse rejects, not fatal.
                s.count(Count::RejectedParse);
            }
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Message::Close(_) => return SessionEnd::Closed,
            Message::Binary(b) => {
                // Tolerate binary-encoded JSON.
                let _ = serde_json::from_slice::<Json>(&b);
                s.count(Count::RejectedParse);
            }
            // ExpectedAbsent: raw frames never surface on read; pong answers
            // our keepalive ping.
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn parse_event(v: &Json, expected_chain: u64, s: &Shared) -> Option<BackrunFeedEvent> {
    let Some(chain_id) = hex_u64(v.get("chainId")) else {
        s.count(Count::RejectedParse);
        return None;
    };
    if chain_id != expected_chain {
        s.count(Count::RejectedChainId);
        return None;
    }
    let parse_key_id = |k: &str| -> Option<Address> { Address::from_hex(v.get(k)?.as_str()?).ok() };
    let Some(from) = parse_key_id("from") else {
        s.count(Count::RejectedParse);
        return None;
    };
    let to = match v.get("to") {
        Some(Json::Null) | None => None,
        Some(x) => {
            let Some(a) = Address::from_hex(x.as_str()?).ok() else {
                s.count(Count::RejectedParse);
                return None;
            };
            Some(a)
        }
    };
    let Some(hash) = v
        .get("hash")
        .and_then(Json::as_str)
        .and_then(|h| B256::from_hex(h).ok())
    else {
        s.count(Count::RejectedParse);
        return None;
    };
    let data = match v.get("data") {
        None | Some(Json::Null) => Bytes::new(),
        Some(x) => {
            let Some(b) = alloy::hex::decode(x.as_str()?.strip_prefix("0x")?).ok() else {
                s.count(Count::RejectedParse);
                return None;
            };
            Bytes::from(b)
        }
    };
    let value = match v.get("value") {
        None | Some(Json::Null) => U256::ZERO,
        Some(x) => {
            let Some(v) = hex_u256(x.as_str()?) else {
                s.count(Count::RejectedParse);
                return None;
            };
            v
        }
    };
    Some(BackrunFeedEvent {
        chain_id,
        from,
        to,
        value,
        data,
        gas: hex_u64(v.get("gas")).unwrap_or_default(),
        max_fee_per_gas: hex_u128(v.get("maxFeePerGas")).unwrap_or_default(),
        max_priority_fee_per_gas: hex_u128(v.get("maxPriorityFeePerGas")).unwrap_or_default(),
        nonce: hex_u64(v.get("nonce")).unwrap_or_default(),
        hash,
        access_list: v.get("accessList").cloned().unwrap_or(Json::Null),
        tx_type: hex_u64(v.get("type"))
            .and_then(|t| u8::try_from(t).ok())
            .unwrap_or_default(),
        received_unix_ms: now_unix_ms(),
        raw_signed_tx: None,
    })
}

fn hex_u64(v: Option<&Json>) -> Option<u64> {
    hex_int(v, |s| u64::from_str_radix(s, 16).ok())
}

fn hex_u128(v: Option<&Json>) -> Option<u128> {
    hex_int(v, |s| u128::from_str_radix(s, 16).ok())
}

fn hex_int<T>(v: Option<&Json>, f: fn(&str) -> Option<T>) -> Option<T> {
    let s = v?.as_str()?;
    f(s.strip_prefix("0x").unwrap_or(s))
}

fn hex_u256(s: &str) -> Option<U256> {
    U256::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}
