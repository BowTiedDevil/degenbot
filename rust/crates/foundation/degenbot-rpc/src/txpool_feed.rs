//! Node txpool pending-transaction feed.
//!
//! Connects a WebSocket to the CHAIN NODE, subscribes to
//! `newPendingTransactions`, and resolves each delivered hash with
//! `eth_getTransactionByHash` over the same socket to build the full pending
//! tx. Typed [`PendingTx`](degenbot_eventhub::PendingTx) frames are published
//! on the source kind's bounded ring by the feed's
//! [`PendingTxPublisher`](degenbot_eventhub::PendingTxPublisher) — the same
//! vocabulary the `MEVBlocker` feed publishes, so the frame pipeline is
//! feed-agnostic. The ring's one view belongs to the arm's stream
//! (`degenbot_rpc::pending_tx_stream`).
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
//! Robustness is the shared feed pump's (`crate::feed_pump`): a frame-silent
//! socket is torn down and reconnected, any server close reconnects with the
//! capped backoff curve that resets once a session proves it can deliver
//! events. Non-1 chain ids and malformed payloads are counted, never fatal.
//! This module owns only the txpool wire — the subscribe method, the hash
//! notice and its by-hash fetch, and the typed frame.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, B256};
use futures_util::SinkExt;
use serde_json::Value as Json;
use tokio_tungstenite::tungstenite::Message;

use degenbot_core::{op_info, op_warn};
use degenbot_eventhub::{PendingTx, PendingTxPublisher};

use crate::feed_pump::{
    hex_u128, hex_u256, hex_u64, now_unix_ms, run_loop, str_field, Count, FeedSession,
    FrameVerdict, PumpConfig, PumpCounters, PumpShared, WsStream,
};
use crate::feed_shell::{spawn_feed_loop, DeferredFeedShell};

/// The txpool feed's production ring capacity: 4096 frames.
pub const DEFAULT_RING_CAPACITY: NonZeroUsize = NonZeroUsize::new(4096).expect("4096 is non-zero");

const SUBSCRIBE_METHOD: &str = "newPendingTransactions";
/// First auto id for a hash fetch (the subscribe ack owns `SUBSCRIBE_ID`).
const FETCH_ID_BASE: u64 = 2;

#[derive(Debug, Clone)]
pub struct TxpoolFeedConfig {
    pub ws_url: String,
    pub expected_chain_id: u64,
    /// Tear down + reconnect when no frame (the subscribe ack included)
    /// arrives within the window.
    pub watchdog: Duration,
    pub ring_capacity: NonZeroUsize,
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
            ring_capacity: DEFAULT_RING_CAPACITY,
            reconnect_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
        }
    }
}

/// Handle over a txpool feed pump.
///
/// [`Self::build`] provisions the pump (the publisher, the shared counters,
/// the stop channel) and spawns NOTHING; [`Self::start`] launches the
/// reconnect loop, which is the first dial. This split is what lets a host
/// mint a feed without touching the network — the deferred-I/O discipline the
/// engine's start boundary relies on.
///
/// The pump owns only the ring's publishing end: a hosted arm drains through
/// its one `PendingTxStream`, and no feed-level `drain` exists to compete with
/// that stream.
pub struct TxpoolFeed {
    /// The shared lifecycle shell (the one-shot latch, the stop channel, the
    /// launch dispatch); declared first so its `Drop` — the stop send — runs
    /// before the pump's other fields tear down.
    shell: DeferredFeedShell,
    shared: Arc<PumpShared>,
    /// The provisioning config [`Self::start`] consumes to spawn the loop.
    /// Held (not consumed at build) so provisioning stays dial-free.
    cfg: TxpoolFeedConfig,
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

impl From<PumpCounters> for TxpoolFeedStatus {
    fn from(c: PumpCounters) -> Self {
        Self {
            connected: c.connected,
            accepted: c.accepted,
            dropped_ring: c.dropped_ring,
            rejected_chain_id: c.rejected_chain_id,
            rejected_parse: c.rejected_parse,
            mine_misses: c.mine_misses,
            reconnects: c.reconnects,
            last_event_unix_ms: c.last_event_unix_ms,
        }
    }
}

impl TxpoolFeed {
    /// Provision the pump over its ring's publishing end, spawning nothing.
    ///
    /// The ring is opened by whoever owns the consumer end: a process host
    /// opens it on its [`degenbot_eventhub::Hub`]
    /// (`degenbot_rpc::pending_tx_stream` owns that provisioning, and is the
    /// txpool arm's only production drain). The feed owns only the publisher.
    /// [`Self::start`] launches the reconnect loop later.
    pub(crate) fn build(cfg: TxpoolFeedConfig, source: PendingTxPublisher) -> Self {
        Self {
            shell: DeferredFeedShell::new(),
            shared: Arc::new(PumpShared::new(source)),
            cfg,
        }
    }

    /// Launch the reconnect loop — the pump's first dial — exactly once. The
    /// loop's stop receiver is subscribed from the stop channel here, so a
    /// [`Self::stop`] before or after the start is honored. Spawns on the
    /// current tokio runtime when one is entered, else the crate-global
    /// runtime.
    pub(crate) fn start(&self) {
        let pump = PumpConfig::new(
            self.cfg.ws_url.clone(),
            self.cfg.watchdog,
            self.cfg.reconnect_backoff,
            self.cfg.max_backoff,
        );
        let shared = Arc::clone(&self.shared);
        let wire = TxpoolWire::new(self.cfg.expected_chain_id);
        self.shell.start_once(move |stop_rx| {
            spawn_feed_loop(async move { run_loop(pump, shared, wire, stop_rx).await });
        });
    }

    #[must_use]
    pub fn status(&self) -> TxpoolFeedStatus {
        self.shared.counters().into()
    }

    /// Politely stop the pump (idempotent).
    pub fn stop(&self) {
        self.shell.stop();
    }
}

/// The chain-node txpool wire: a hash notice resolved by an on-socket
/// `eth_getTransactionByHash` fetch (a full-tx notice needs no fetch).
struct TxpoolWire {
    expected_chain_id: u64,
    /// In-flight hash fetches: request id -> tx hash.
    fetches: HashMap<u64, B256>,
    next_fetch_id: u64,
}

impl TxpoolWire {
    fn new(expected_chain_id: u64) -> Self {
        Self {
            expected_chain_id,
            fetches: HashMap::new(),
            next_fetch_id: FETCH_ID_BASE,
        }
    }
}

impl FeedSession for TxpoolWire {
    const LABEL: &'static str = "txpool feed";
    const SUBSCRIBE_METHOD: &'static str = SUBSCRIBE_METHOD;

    async fn handle_frame(
        &mut self,
        ws: &mut WsStream,
        shared: &PumpShared,
        sub_id: Option<&str>,
        v: Json,
    ) -> FrameVerdict {
        // By-hash fetch response: the tx object (or null = mined first).
        if v.get("method").is_none() {
            let resolved = v
                .get("id")
                .and_then(Json::as_u64)
                .and_then(|id| self.fetches.remove(&id).map(|hash| (id, hash)));
            if let Some((_, hash)) = resolved {
                match v.get("result") {
                    Some(Json::Null) | None => {
                        shared.count(Count::MineMisses);
                    }
                    Some(result) => {
                        if let Some(ev) =
                            parse_full_tx(result, hash, self.expected_chain_id, shared)
                        {
                            op_info!(
                                domain = rpc,
                                hash = %ev.hash,
                                from = %ev.from,
                                to = ?ev.to,
                                chain_id = ev.chain_id,
                                gas = ev.gas,
                                nonce = ev.nonce,
                                "{}: pending tx", Self::LABEL
                            );
                            shared.push(ev);
                        }
                    }
                }
                return FrameVerdict::Continue;
            }
            shared.count(Count::RejectedParse);
            return FrameVerdict::Continue;
        }
        // Subscription notice: the hash (or, on nodes that send full
        // bodies, the tx object itself).
        let is_sub_notif = v.get("method").and_then(Json::as_str) == Some("eth_subscription");
        let sub_matches = sub_id
            .zip(v.pointer("/params/subscription").and_then(Json::as_str))
            .is_some_and(|(a, b)| a == b);
        if !(is_sub_notif && sub_matches) {
            shared.count(Count::RejectedParse);
            return FrameVerdict::Continue;
        }
        let Some(result) = v.pointer("/params/result") else {
            shared.count(Count::RejectedParse);
            return FrameVerdict::Continue;
        };
        match result {
            Json::String(hash_str) => {
                let Ok(hash) = hash_str
                    .strip_prefix("0x")
                    .unwrap_or(hash_str)
                    .parse::<B256>()
                else {
                    shared.count(Count::RejectedParse);
                    return FrameVerdict::Continue;
                };
                let id = self.next_fetch_id;
                self.next_fetch_id = self.next_fetch_id.wrapping_add(1).max(FETCH_ID_BASE);
                self.fetches.insert(id, hash);
                let fetch_req = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "eth_getTransactionByHash",
                    "params": [hash]
                });
                if let Err(e) = ws.send(Message::Text(fetch_req.to_string().into())).await {
                    op_warn!(domain = rpc, %e, "{}: fetch send failed", Self::LABEL);
                    return FrameVerdict::Closed;
                }
            }
            // A node delivering full bodies straight on the sub: parse it.
            Json::Object(_) => {
                let Some(hash) = result.get("hash").and_then(|h| {
                    h.as_str()
                        .and_then(|s| s.strip_prefix("0x").unwrap_or(s).parse::<B256>().ok())
                }) else {
                    shared.count(Count::RejectedParse);
                    return FrameVerdict::Continue;
                };
                if let Some(ev) = parse_full_tx(result, hash, self.expected_chain_id, shared) {
                    shared.push(ev);
                }
            }
            _ => shared.count(Count::RejectedParse),
        }
        FrameVerdict::Continue
    }
}

fn parse_full_tx(
    v: &Json,
    hash: B256,
    expected_chain_id: u64,
    shared: &PumpShared,
) -> Option<PendingTx> {
    let Some(from) = str_field(v, "from").and_then(|x| x.parse::<Address>().ok()) else {
        shared.count(Count::RejectedParse);
        return None;
    };
    let chain_id = hex_u64(v.get("chainId")).unwrap_or_default();
    if chain_id != expected_chain_id {
        shared.count(Count::RejectedChainId);
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
            shared.count(Count::RejectedParse);
            return None;
        };
        (price, price)
    };
    Some(PendingTx {
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
