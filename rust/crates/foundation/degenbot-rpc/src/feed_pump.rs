//! The pending-tx feed pump: one reconnect loop for both pending-tx feeds.
//!
//! The `MEVBlocker` searcher feed and the chain-node txpool feed are one
//! pump: the deferred-start shell (`crate::feed_shell`), the telemetry
//! counters, the watchdog/reconnect loop, and the capped backoff curve that
//! resets once a session proves it can deliver events all live here once. The
//! only variance between the feeds is their wire protocol — the subscribe
//! method and what a notification frame means — and that variance is
//! [`FeedSession`].
//!
//! The reconnect curve is the workspace `RetryPolicy` curve
//! ([`degenbot_core::retry::RetryPolicy`]), consumed as a curve only: the
//! pump dials forever, so the policy's attempt bound is unused (the same
//! posture as `subscription`'s `newHeads` reconnect).

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::U256;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value as Json;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use degenbot_core::retry::RetryPolicy;
use degenbot_core::{op_info, op_warn};
use degenbot_eventhub::{PendingTx, PendingTxPublisher};

/// The subscribe ack's request id, which the server mirrors.
pub(crate) const SUBSCRIBE_ID: u64 = 1;

/// The socket one feed session pumps: a WebSocket over the transport
/// `connect_async` established.
pub(crate) type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The pump's provisioning config: where to dial, the stall watchdog, and
/// the reconnect curve.
#[derive(Clone, Debug)]
pub(crate) struct PumpConfig {
    url: String,
    watchdog: Duration,
    reconnect: ReconnectBackoff,
}

impl PumpConfig {
    /// Provision a pump config over its dial target and its two backoff
    /// knobs: `initial_backoff` waits before the first redial, doubling per
    /// consecutive failure up to `max_backoff`.
    pub(crate) fn new(
        url: String,
        watchdog: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            url,
            watchdog,
            reconnect: ReconnectBackoff::new(initial_backoff, max_backoff),
        }
    }
}

/// The pump's reconnect waits: the shared [`RetryPolicy`] curve plus the
/// pump's sequencing — one wait per dial attempt, and a delivering session
/// resets the sequence. The policy is consumed as a curve only (the pump
/// dials forever), so its attempt bound is unused.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReconnectBackoff {
    curve: RetryPolicy,
    attempt: u32,
}

impl ReconnectBackoff {
    /// The curve over two duration knobs: `initial` waits before the first
    /// redial, doubling per consecutive failure up to `cap`.
    pub(crate) fn new(initial: Duration, cap: Duration) -> Self {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        Self {
            curve: RetryPolicy::from_millis(u32::MAX, ms(initial), ms(cap), 0),
            attempt: 0,
        }
    }

    /// The wait before the next dial: the base delay for the first dial
    /// after a reset, doubling per consecutive failure up to the cap.
    pub(crate) fn next_wait(&mut self) -> Duration {
        self.attempt += 1;
        self.curve.capped_backoff(self.attempt)
    }

    /// A session that delivered events proved the transport: the next wait
    /// starts again at the base delay.
    pub(crate) fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// The counters both feeds publish, over the ring's publishing end.
pub(crate) struct PumpShared {
    /// The publishing end of the feed's ring (the ring owns the capacity and
    /// the eviction counter the feed reports).
    source: PendingTxPublisher,
    connected: AtomicBool,
    accepted: AtomicU64,
    rejected_chain_id: AtomicU64,
    rejected_parse: AtomicU64,
    mine_misses: AtomicU64,
    reconnects: AtomicU64,
    last_event_unix_ms: AtomicU64,
}

impl PumpShared {
    /// Mint the counters over a ring's publishing end. Nothing is dialed.
    pub(crate) fn new(source: PendingTxPublisher) -> Self {
        Self {
            source,
            connected: AtomicBool::new(false),
            accepted: AtomicU64::new(0),
            rejected_chain_id: AtomicU64::new(0),
            rejected_parse: AtomicU64::new(0),
            mine_misses: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            last_event_unix_ms: AtomicU64::new(0),
        }
    }

    /// Publish a typed frame: count it, stamp it, and hand it to the ring.
    pub(crate) fn push(&self, ev: PendingTx) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix_ms
            .store(now_unix_ms(), Ordering::Relaxed);
        self.source.push(ev);
    }

    /// Advance one classification counter.
    pub(crate) fn count(&self, which: Count) {
        let c = match which {
            Count::RejectedChainId => &self.rejected_chain_id,
            Count::RejectedParse => &self.rejected_parse,
            Count::MineMisses => &self.mine_misses,
            Count::Reconnects => &self.reconnects,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// One snapshot of every counter, which a feed's status maps from.
    pub(crate) fn counters(&self) -> PumpCounters {
        PumpCounters {
            connected: self.connected.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            dropped_ring: self.source.dropped(),
            rejected_chain_id: self.rejected_chain_id.load(Ordering::Relaxed),
            rejected_parse: self.rejected_parse.load(Ordering::Relaxed),
            mine_misses: self.mine_misses.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            last_event_unix_ms: self.last_event_unix_ms.load(Ordering::Relaxed),
        }
    }
}

/// Which counter a wire classification advances.
#[derive(Clone, Copy)]
pub(crate) enum Count {
    RejectedChainId,
    RejectedParse,
    MineMisses,
    Reconnects,
}

/// One snapshot of the pump counters; the shared vocabulary both feeds'
/// public status structs map from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PumpCounters {
    pub(crate) connected: bool,
    pub(crate) accepted: u64,
    pub(crate) dropped_ring: u64,
    pub(crate) rejected_chain_id: u64,
    pub(crate) rejected_parse: u64,
    pub(crate) mine_misses: u64,
    pub(crate) reconnects: u64,
    pub(crate) last_event_unix_ms: u64,
}

/// How a session ended: the host stopped it, the server closed it, or the
/// watchdog reaped a frame-silent socket.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SessionEnd {
    Stopped,
    Closed,
    Stall,
    /// The peer closed the socket and supplied its own close evidence, if
    /// any was present on the frame.
    PeerClose {
        code: Option<u16>,
        reason: Option<String>,
    },
}

impl SessionEnd {
    /// The disconnect event's classification plus whatever close evidence the
    /// peer supplied; the ends that carry no peer evidence report none.
    fn disconnect_evidence(&self) -> (&'static str, Option<u16>, Option<&str>) {
        match self {
            Self::Stall => ("stall", None, None),
            Self::PeerClose { code, reason } => ("peer close", *code, reason.as_deref()),
            Self::Stopped | Self::Closed => ("closed", None, None),
        }
    }
}

/// What a wire handler wants after one frame: keep reading, or close the
/// session (a reply it must send could not be written).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FrameVerdict {
    Continue,
    Closed,
}

/// One feed's wire protocol: the subscribe method it handshakes with and
/// what a notification frame means on it. The pump owns everything else —
/// the dial, the watchdog, the reconnect curve, the counters.
pub(crate) trait FeedSession: Send + 'static {
    /// The log label naming the feed (`"backrun feed"`, `"txpool feed"`).
    const LABEL: &'static str;
    /// The `eth_subscribe` method this feed handshakes with.
    const SUBSCRIBE_METHOD: &'static str;

    /// Handle one parsed JSON frame that is not the subscribe ack. `sub_id`
    /// is the subscription id the ack carried, once it has arrived; `ws` is
    /// the session's socket, for a wire that issues requests of its own.
    fn handle_frame(
        &mut self,
        ws: &mut WsStream,
        shared: &PumpShared,
        sub_id: Option<&str>,
        frame: Json,
    ) -> impl Future<Output = FrameVerdict> + Send;
}

/// The one clock read both feeds stamp frames with. A clock before the epoch
/// reports 0 and a millisecond count past `u64` reports `u64::MAX`: one clamp
/// policy, so the two feeds' stamps always compare the same way.
pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// The JSON hex-field parses the pending-tx wires share: `0x` optional,
/// radix 16, `None` on any shape or digit failure.
pub(crate) fn str_field<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Json::as_str)
}

/// [`str_field`]'s u64 read.
pub(crate) fn hex_u64(v: Option<&Json>) -> Option<u64> {
    let s = v?.as_str()?;
    u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// [`str_field`]'s u128 read.
pub(crate) fn hex_u128(v: Option<&Json>) -> Option<u128> {
    let s = v?.as_str()?;
    u128::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// [`str_field`]'s `U256` read.
pub(crate) fn hex_u256(s: &str) -> Option<U256> {
    U256::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
}

/// The pump's reconnect loop: dial, run one session, wait the reconnect
/// curve, repeat — until the host stops it. A session that delivered events
/// resets the curve before the next wait.
pub(crate) async fn run_loop<S: FeedSession>(
    cfg: PumpConfig,
    shared: Arc<PumpShared>,
    mut wire: S,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut reconnect = cfg.reconnect;
    loop {
        if *stop_rx.borrow() {
            return;
        }
        shared.connected.store(false, Ordering::Relaxed);
        let request = match cfg.url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                op_warn!(domain = rpc, url = %cfg.url, e = %e, "{}: invalid url", S::LABEL);
                return;
            }
        };
        let socket = match tokio::time::timeout(cfg.watchdog, connect_async(request)).await {
            Ok(Ok((ws, _resp))) => {
                op_info!(domain = rpc, url = %cfg.url, "{}: connected", S::LABEL);
                ws
            }
            Ok(Err(e)) => {
                shared.count(Count::Reconnects);
                op_warn!(domain = rpc, %e, "{}: connect failed", S::LABEL);
                tokio::time::sleep(reconnect.next_wait()).await;
                continue;
            }
            Err(_) => {
                shared.count(Count::Reconnects);
                op_warn!(domain = rpc, "{}: connect timeout", S::LABEL);
                tokio::time::sleep(reconnect.next_wait()).await;
                continue;
            }
        };
        let accepted_before = shared.accepted.load(Ordering::Relaxed);
        match run_session(socket, &shared, cfg.watchdog, &mut wire, &mut stop_rx).await {
            SessionEnd::Stopped => {
                op_info!(domain = rpc, url = %cfg.url, "{}: stopped", S::LABEL);
                return;
            }
            end @ (SessionEnd::Closed | SessionEnd::Stall | SessionEnd::PeerClose { .. }) => {
                let session_accepted = shared.accepted.load(Ordering::Relaxed) - accepted_before;
                if session_accepted > 0 {
                    reconnect.reset();
                }
                shared.connected.store(false, Ordering::Relaxed);
                shared.count(Count::Reconnects);
                let (reason, close_code, close_reason) = end.disconnect_evidence();
                op_info!(
                    domain = rpc,
                    url = %cfg.url,
                    reason,
                    close_code = ?close_code,
                    close_reason = ?close_reason,
                    session_accepted,
                    reconnects = shared.reconnects.load(Ordering::Relaxed),
                    "{}: disconnected", S::LABEL
                );
                tokio::time::sleep(reconnect.next_wait()).await;
            }
        }
    }
}

/// One connected session: the subscribe handshake, then frames until the
/// wire closes it, the server closes the socket, the watchdog reaps a
/// frame-silent socket, or the host stops the pump.
async fn run_session<S: FeedSession>(
    mut ws: WsStream,
    shared: &PumpShared,
    watchdog: Duration,
    wire: &mut S,
    stop_rx: &mut watch::Receiver<bool>,
) -> SessionEnd {
    let sub_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": SUBSCRIBE_ID,
        "method": "eth_subscribe",
        "params": [S::SUBSCRIBE_METHOD]
    });
    if let Err(e) = ws.send(Message::Text(sub_req.to_string().into())).await {
        op_warn!(domain = rpc, %e, "{}: subscribe send failed", S::LABEL);
        return SessionEnd::Closed;
    }
    let mut sub_id: Option<String> = None;
    shared.connected.store(true, Ordering::Relaxed);
    loop {
        let frame = tokio::select! {
            () = tokio::time::sleep(watchdog) => return SessionEnd::Stall,
            // Wildcard `_` is deliberate: `changed()` also yields `Err` on a dropped stop sender, and this arm must fire then too so a closed watch ends the session promptly instead of waiting out the watchdog.
            _ = stop_rx.changed() => {
                let _ = ws.send(Message::Close(None)).await;
                return SessionEnd::Stopped;
            }
            f = ws.next() => match f {
                None => return SessionEnd::Closed,
                Some(Err(e)) => {
                    op_warn!(domain = rpc, %e, "{}: stream error", S::LABEL);
                    return SessionEnd::Closed;
                }
                Some(Ok(m)) => m,
            },
        };
        match frame {
            Message::Text(t) => {
                let Ok(v): Result<Json, _> = serde_json::from_str(t.as_str()) else {
                    shared.count(Count::RejectedParse);
                    continue;
                };
                // Subscribe ack: mirror id, string result, no `method` field.
                if v.get("method").is_none()
                    && v.get("id").and_then(Json::as_u64) == Some(SUBSCRIBE_ID)
                {
                    if let Some(id) = v.get("result").and_then(Json::as_str) {
                        sub_id = Some(id.to_string());
                        op_info!(domain = rpc, subscription = %id, "{}: subscribed", S::LABEL);
                    }
                    continue;
                }
                match wire
                    .handle_frame(&mut ws, shared, sub_id.as_deref(), v)
                    .await
                {
                    FrameVerdict::Continue => {}
                    FrameVerdict::Closed => return SessionEnd::Closed,
                }
            }
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Message::Close(frame) => {
                return SessionEnd::PeerClose {
                    code: frame.as_ref().map(|f| u16::from(f.code)),
                    reason: frame
                        .as_ref()
                        .map(|f| f.reason.as_str())
                        .filter(|r| !r.is_empty())
                        .map(str::to_owned),
                };
            }
            Message::Binary(_) => shared.count(Count::RejectedParse),
            // ExpectedAbsent: raw frames never surface on read; pong answers
            // our keepalive ping.
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use std::num::NonZeroUsize;

    use tokio_tungstenite::accept_async;

    use degenbot_eventhub::detached_pending_ring;

    use super::*;

    /// The pump's reconnect contract, as literals derived from the curve
    /// spec: the wait before each dial starts at the base delay and doubles
    /// per failed dial up to the cap, and a delivering session resets the
    /// sequence back to the base delay.
    #[test]
    fn reconnect_waits_double_and_cap_and_reset_on_delivery() {
        let mut reconnect =
            ReconnectBackoff::new(Duration::from_millis(20), Duration::from_millis(50));
        assert_eq!(reconnect.next_wait(), Duration::from_millis(20));
        assert_eq!(reconnect.next_wait(), Duration::from_millis(40));
        assert_eq!(reconnect.next_wait(), Duration::from_millis(50));
        assert_eq!(reconnect.next_wait(), Duration::from_millis(50));
        reconnect.reset();
        assert_eq!(reconnect.next_wait(), Duration::from_millis(20));
    }

    /// A wire that reads whatever it is handed and never ends the session.
    struct SilentWire;

    impl FeedSession for SilentWire {
        const LABEL: &'static str = "test feed";
        const SUBSCRIBE_METHOD: &'static str = "test_subscription";

        fn handle_frame(
            &mut self,
            _ws: &mut WsStream,
            _shared: &PumpShared,
            _sub_id: Option<&str>,
            _frame: Json,
        ) -> impl Future<Output = FrameVerdict> + Send {
            std::future::ready(FrameVerdict::Continue)
        }
    }

    /// Acceptance pin for the stop arm of the session frame `select!` (the
    /// `_ = stop_rx.changed()` arm just above). `watch::Receiver::changed()`
    /// resolves to `Err` when the stop sender is dropped WITHOUT a send; the
    /// wildcard binding makes the arm fire on that `Err` too, so a closed stop
    /// watch ends the session promptly as `SessionEnd::Stopped`. An
    /// `Ok(()) = stop_rx.changed()` binding leaves the arm disabled in that
    /// case and the session waits out the watchdog (returning `Stall` here).
    ///
    /// Shape: this drives the real `run_session` directly against a silent
    /// mock WS, because the public feed APIs cannot drop the sender without a
    /// send — both `stop()` and `Drop` call `stop_tx.send(true)`, an `Ok`
    /// change, never the sender-drop this arm exists to catch.
    #[tokio::test]
    async fn dropped_stop_sender_ends_the_session_as_stopped() {
        // Mock server: accept one connection and hold it open, relaying
        // nothing — so the ws.next() arm stays pending.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(mut ws) = accept_async(stream).await {
                    while let Some(Ok(_)) = ws.next().await {}
                }
            }
        });
        let ws_url = format!("ws://127.0.0.1:{}", addr.port());
        let request = ws_url.as_str().into_client_request().unwrap();
        let (ws, _resp) = connect_async(request).await.unwrap();

        let (source, _ring) = detached_pending_ring(NonZeroUsize::new(8).unwrap());
        let shared = PumpShared::new(source);
        let (stop_tx, mut stop_rx) = watch::channel(false);
        // Sender dropped with no send: the receiver can only observe the
        // closed channel, so `changed()` resolves to `Err`.
        drop(stop_tx);

        let mut wire = SilentWire;
        let end = run_session(ws, &shared, Duration::from_secs(2), &mut wire, &mut stop_rx).await;
        assert_eq!(
            end,
            SessionEnd::Stopped,
            "a dropped stop sender must end the session as Stopped, not wait out the watchdog"
        );
    }
}
