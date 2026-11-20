//! `MEVBlocker` searcher feed client.
//!
//! Connects to `<wss://searchers.mevblocker.io>`, subscribes to
//! `mevblocker_partialPendingTransactions`, and publishes typed
//! [`PendingTx`](degenbot_eventhub::PendingTx) frames on its source kind's
//! bounded ring ([`PendingTxPublisher`]): the publisher owns the eviction
//! counter the feed reports, and the ring's one view
//! ([`degenbot_eventhub::PendingTxView`]) belongs to whoever drains it —
//! [`EagerBackrunFeed`] for the single-feed eager path, a
//! `degenbot_rpc::pending_tx_stream::PendingTxStream` for a hosted arm.
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

use std::num::NonZeroUsize;
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
use degenbot_eventhub::{detached_pending_ring, PendingTx, PendingTxPublisher, PendingTxView};

use crate::feed_shell::{spawn_feed_loop, DeferredFeedShell};

/// The default searcher WS; re-exported so the readiness resolution and
/// this crate share one constant.
pub use degenbot_config::DEFAULT_BACKRUN_STREAM_URL as DEFAULT_STREAM_URL;
pub const BACKRUN_SUBSCRIPTION_METHOD: &str = "mevblocker_partialPendingTransactions";
/// The production pending-ring capacity: 4096 frames per source kind.
///
/// A ring is always bounded — the capacity is a [`NonZeroUsize`], so the
/// zero-capacity ring a caller could once request is unrepresentable.
pub const DEFAULT_RING_CAPACITY: NonZeroUsize = NonZeroUsize::new(4096).expect("4096 is non-zero");

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
    pub ring_capacity: NonZeroUsize,
    pub reconnect_backoff: Duration,
    pub max_backoff: Duration,
}

/// Why a caller-supplied ring capacity was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RingCapacityError {
    /// A ring must hold at least one frame: an explicit zero is refused, never
    /// silently replaced by the default.
    #[error("ring capacity must be at least one frame; omit it to select the default")]
    Zero,
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
            ring_capacity: DEFAULT_RING_CAPACITY,
            reconnect_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
        }
    }

    /// Apply a caller-supplied ring capacity: an absent value keeps this
    /// config's default, and an explicit zero is refused rather than absorbed.
    ///
    /// # Errors
    ///
    /// [`RingCapacityError::Zero`] when `capacity` is `Some(0)`.
    pub fn with_ring_capacity(
        mut self,
        capacity: Option<usize>,
    ) -> Result<Self, RingCapacityError> {
        if let Some(capacity) = capacity {
            self.ring_capacity = NonZeroUsize::new(capacity).ok_or(RingCapacityError::Zero)?;
        }
        Ok(self)
    }
}

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
    /// The publishing end of this source kind's ring (the ring owns the
    /// capacity and the eviction counter the feed reports).
    source: PendingTxPublisher,
    connected: AtomicBool,
    accepted: AtomicU64,
    rejected_chain_id: AtomicU64,
    rejected_parse: AtomicU64,
    reconnects: AtomicU64,
    last_event_unix_ms: AtomicU64,
}

impl Shared {
    fn push(&self, ev: PendingTx) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix_ms
            .store(now_unix_ms(), Ordering::Relaxed);
        self.source.push(ev);
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

/// Handle over a feed pump.
///
/// [`Self::build`] provisions the pump (the ring ends, the shared counters,
/// the stop channel) and spawns NOTHING; [`Self::start`] launches the
/// reconnect loop, which is the first dial. This split is what lets a host
/// mint a feed without touching the network — the deferred-I/O discipline the
/// engine's start boundary relies on.
pub struct BackrunFeed {
    /// The shared lifecycle shell (the one-shot latch, the stop channel, the
    /// launch dispatch); declared first so its `Drop` — the stop send — runs
    /// before the pump's other fields tear down.
    shell: DeferredFeedShell,
    shared: Arc<Shared>,
    /// The provisioning config [`Self::start`] consumes to spawn the loop.
    /// Held (not consumed at build) so provisioning stays dial-free.
    cfg: BackrunFeedConfig,
}

impl BackrunFeed {
    /// Provision the pump over its ring's publishing end, spawning nothing.
    ///
    /// The ring is opened by whoever owns the consumer end: a process host
    /// opens it on its [`degenbot_eventhub::Hub`]
    /// (`degenbot_rpc::pending_tx_stream` owns that provisioning), and the
    /// eager single-feed path opens a detached one ([`EagerBackrunFeed`]). The
    /// feed owns only the publisher. [`Self::start`] launches the reconnect
    /// loop later.
    pub(crate) fn build(cfg: BackrunFeedConfig, source: PendingTxPublisher) -> Self {
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
        Self {
            shell: DeferredFeedShell::new(),
            shared,
            cfg,
        }
    }

    /// Launch the reconnect loop — the pump's first dial — exactly once. The
    /// loop's stop receiver is subscribed from the stop channel here, so a
    /// [`Self::stop`] before or after the start is honored. Spawns on the
    /// current tokio runtime when one is entered, else the crate-global
    /// runtime.
    pub(crate) fn start(&self) {
        let cfg = self.cfg.clone();
        let sh = Arc::clone(&self.shared);
        self.shell.start_once(move |stop_rx| {
            spawn_feed_loop(async move { run_loop(cfg, sh, stop_rx).await });
        });
    }

    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        let s = &self.shared;
        BackrunFeedStatus {
            connected: s.connected.load(Ordering::Relaxed),
            accepted: s.accepted.load(Ordering::Relaxed),
            dropped_ring: s.source.dropped(),
            rejected_chain_id: s.rejected_chain_id.load(Ordering::Relaxed),
            rejected_parse: s.rejected_parse.load(Ordering::Relaxed),
            reconnects: s.reconnects.load(Ordering::Relaxed),
            last_event_unix_ms: s.last_event_unix_ms.load(Ordering::Relaxed),
        }
    }

    /// Politely stop the pump (idempotent).
    pub fn stop(&self) {
        self.shell.stop();
    }
}

/// The eager single-feed path: one detached ring, one feed, one drainer.
///
/// The Python `degenbot._ffi.backrun` binding is this adapter's production
/// caller. The feed is its own host, so it opens a detached ring instead of
/// joining a process hub's registry — and this struct holds that ring's one
/// [`PendingTxView`], so the eager path has no optional view and no hosted
/// drain branch to fall through.
pub struct EagerBackrunFeed {
    feed: BackrunFeed,
    view: PendingTxView,
}

impl EagerBackrunFeed {
    /// Provision a detached ring at the config's capacity and launch the feed
    /// loop — the eager dial. Must be called within a tokio runtime context, or
    /// the crate-global `degenbot_core::runtime::get_runtime()` is used.
    #[must_use]
    pub fn spawn(cfg: BackrunFeedConfig) -> Self {
        let (publisher, view) = detached_pending_ring(cfg.ring_capacity);
        let feed = BackrunFeed::build(cfg, publisher);
        feed.start();
        Self { feed, view }
    }

    /// All buffered frames, ordered, atomically (rings swap; concurrent drains
    /// may interleave but never duplicate or lose between both buffers).
    #[must_use]
    pub fn drain(&self) -> Vec<PendingTx> {
        self.view.drain()
    }

    /// The ring capacity this feed drains.
    #[must_use]
    pub fn capacity(&self) -> NonZeroUsize {
        self.view.capacity()
    }

    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        self.feed.status()
    }

    /// Politely stop the pump (idempotent).
    pub fn stop(&self) {
        self.feed.stop();
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
            // Wildcard `_` is deliberate: `changed()` also yields `Err` on a dropped stop sender, and this arm must fire then too so a closed watch ends the session promptly instead of waiting out the watchdog.
            _ = stop_rx.changed() => {
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

fn parse_event(v: &Json, expected_chain: u64, s: &Shared) -> Option<PendingTx> {
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
    Some(PendingTx {
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
