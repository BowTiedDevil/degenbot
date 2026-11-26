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
//! Robustness is the shared feed pump's (`crate::feed_pump`): a frame-silent
//! socket is torn down and reconnected, any server close reconnects with the
//! capped backoff curve that resets once a session proves it can deliver
//! events. Non-1 chain ids and malformed notifications are counted, never
//! fatal. This module owns only the `MEVBlocker` wire — the subscribe method,
//! the notification parse, and the typed frame.

use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use alloy::hex::FromHex;
use alloy::primitives::{Address, Bytes, B256, U256};
use serde_json::Value as Json;

use degenbot_core::op_info;
use degenbot_eventhub::{detached_pending_ring, PendingTx, PendingTxPublisher, PendingTxView};

use crate::feed_pump::{
    hex_u128, hex_u256, hex_u64, now_unix_ms, run_loop, Count, FeedSession, FrameVerdict,
    PumpConfig, PumpCounters, PumpShared, WsStream,
};
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

impl From<PumpCounters> for BackrunFeedStatus {
    fn from(c: PumpCounters) -> Self {
        Self {
            connected: c.connected,
            accepted: c.accepted,
            dropped_ring: c.dropped_ring,
            rejected_chain_id: c.rejected_chain_id,
            rejected_parse: c.rejected_parse,
            reconnects: c.reconnects,
            last_event_unix_ms: c.last_event_unix_ms,
        }
    }
}

/// The sampler shape both feeds' statuses converge on: the txpool feed's
/// extra `mine_misses` counter is pump-scoped forensics, not an engine
/// instrument.
impl From<crate::txpool_feed::TxpoolFeedStatus> for BackrunFeedStatus {
    fn from(st: crate::txpool_feed::TxpoolFeedStatus) -> Self {
        Self {
            connected: st.connected,
            accepted: st.accepted,
            dropped_ring: st.dropped_ring,
            rejected_chain_id: st.rejected_chain_id,
            rejected_parse: st.rejected_parse,
            reconnects: st.reconnects,
            last_event_unix_ms: st.last_event_unix_ms,
        }
    }
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
    shared: Arc<PumpShared>,
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
            self.cfg.url.clone(),
            self.cfg.watchdog,
            self.cfg.reconnect_backoff,
            self.cfg.max_backoff,
        );
        let shared = Arc::clone(&self.shared);
        let wire = BackrunWire::new(self.cfg.expected_chain_id);
        self.shell.start_once(move |stop_rx| {
            spawn_feed_loop(async move { run_loop(pump, shared, wire, stop_rx).await });
        });
    }

    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        self.shared.counters().into()
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

/// The `MEVBlocker` wire: a notification's `params.result` is the pending tx.
struct BackrunWire {
    expected_chain_id: u64,
}

impl BackrunWire {
    fn new(expected_chain_id: u64) -> Self {
        Self { expected_chain_id }
    }
}

impl FeedSession for BackrunWire {
    const LABEL: &'static str = "backrun feed";
    const SUBSCRIBE_METHOD: &'static str = BACKRUN_SUBSCRIPTION_METHOD;

    fn handle_frame(
        &mut self,
        _ws: &mut WsStream,
        shared: &PumpShared,
        sub_id: Option<&str>,
        v: Json,
    ) -> impl Future<Output = FrameVerdict> + Send {
        let expected_chain_id = self.expected_chain_id;
        async move {
            let is_sub_notif = v.get("method").and_then(Json::as_str) == Some("eth_subscription");
            let sub_matches = sub_id
                .zip(v.pointer("/params/subscription").and_then(Json::as_str))
                .is_some_and(|(a, b)| a == b);
            if is_sub_notif && sub_matches {
                match v.pointer("/params/result") {
                    Some(result) => {
                        if let Some(ev) = parse_event(result, expected_chain_id, shared) {
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
                    None => shared.count(Count::RejectedParse),
                }
                return FrameVerdict::Continue;
            }
            // Server-initiated frames we do not recognize (e.g. a different
            // subscription id) count as parse rejects, not fatal.
            shared.count(Count::RejectedParse);
            FrameVerdict::Continue
        }
    }
}

fn parse_event(v: &Json, expected_chain: u64, shared: &PumpShared) -> Option<PendingTx> {
    let Some(chain_id) = hex_u64(v.get("chainId")) else {
        shared.count(Count::RejectedParse);
        return None;
    };
    if chain_id != expected_chain {
        shared.count(Count::RejectedChainId);
        return None;
    }
    let parse_key_id = |k: &str| -> Option<Address> { Address::from_hex(v.get(k)?.as_str()?).ok() };
    let Some(from) = parse_key_id("from") else {
        shared.count(Count::RejectedParse);
        return None;
    };
    let to = match v.get("to") {
        Some(Json::Null) | None => None,
        Some(x) => {
            let Some(a) = Address::from_hex(x.as_str()?).ok() else {
                shared.count(Count::RejectedParse);
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
        shared.count(Count::RejectedParse);
        return None;
    };
    let data = match v.get("data") {
        None | Some(Json::Null) => Bytes::new(),
        Some(x) => {
            let Some(b) = alloy::hex::decode(x.as_str()?.strip_prefix("0x")?).ok() else {
                shared.count(Count::RejectedParse);
                return None;
            };
            Bytes::from(b)
        }
    };
    let value = match v.get("value") {
        None | Some(Json::Null) => U256::ZERO,
        Some(x) => {
            let Some(v) = hex_u256(x.as_str()?) else {
                shared.count(Count::RejectedParse);
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
