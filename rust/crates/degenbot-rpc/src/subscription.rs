//! Subscription support for the Alloy provider.
//!
//! Provides the double-buffer pump task infrastructure that reads from Alloy's
//! subscription streams and accumulates raw items in Rust buffers (zero GIL),
//! then bulk-converts them to Python objects on drain.
//!
//! # Design
//!
//! Each subscription spawns a tokio task (the "pump") that:
//! 1. Awaits the Alloy subscription call
//! 2. Reads items from the resulting stream
//! 3. Writes raw items to the active buffer (no GIL needed)
//! 4. Sets a coalesced signal flag and sends a wake-up notification
//!
//! The `PyAlloySubscription` class reads accumulated items via `drain()`:
//! - Atomically swaps the active buffer index (`fetch_xor`)
//! - Takes the stale buffer contents (`mem::take`)
//! - Acquires the GIL once for bulk conversion
//! - Returns a Python list of dicts
//!
//! `__anext__()` uses `drain()` internally with a local batch so that
//! `async for` iteration works naturally.

use crate::provider::AlloyProvider;

use alloy::network::Ethereum;
use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use degenbot_core::runtime::get_runtime;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Maximum elapsed time with no header before the watchdog tears down + reconnects
/// a stalled `newHeads` subscription.
///
/// ~4× the ~12s Ethereum mainnet block time = ~48s, within the ≤~60s target of
/// O3JW5S. A silently half-open socket (NAT idle timeout, provider stall with no
/// close frame) makes `stream.next()` never resolve and never return `None`; this
/// watchdog bounds that window so the pump reconnects instead of hanging forever.
const HEADER_WATCHDOG_SECS: u64 = 48;

/// A boxed, `'static`, `Send` stream of Ethereum block headers.
///
/// Both the initial `subscribe_blocks().into_stream()` and every reconnect yield
/// this same concrete type so [`pump_header_stream`]'s generic `S` unifies across
/// the initial and re-established streams.
type HeaderStream = futures_util::stream::BoxStream<'static, alloy::rpc::types::Header>;

// ---------------------------------------------------------------------------
// Double-buffer subscription handle
// ---------------------------------------------------------------------------

/// Manages the double-buffer and background pump task for a subscription.
///
/// The double-buffer pattern:
/// - `buffers[active]` is where the pump writes (no GIL)
/// - `buffers[stale]` is what `drain()` empties and converts (GIL)
/// - `active` is swapped atomically via `fetch_xor(1)`
/// - `signaled` is a coalesced flag: pump sets true on write, sends wake-up
///   only on false→true transition. Python clears on drain.
pub struct SubscriptionHandle {
    /// Double buffer: raw items.
    pub buffers: [parking_lot::Mutex<Vec<RawSubItem>>; 2],
    /// Index of the active buffer (0 or 1). Pump writes to buffers[active].
    pub active: AtomicUsize,
    /// Coalesced signal flag. Pump sets true on write; Python clears on drain.
    pub signaled: AtomicBool,
    /// Wake-up channel sender: pump sends () when signaled transitions false→true.
    pub notify_tx: mpsc::Sender<()>,
    /// Wake-up channel receiver: Python side takes this and awaits it.
    pub notify_rx: parking_lot::Mutex<Option<mpsc::Receiver<()>>>,
    /// Handle to the pump task — aborted on unsubscribe/drop.
    pub task: parking_lot::Mutex<Option<JoinHandle<()>>>,
    /// Whether the user explicitly unsubscribed.
    pub unsubscribed: AtomicBool,
    /// Set to true when the pump has successfully established the subscription.
    pub started: AtomicBool,
    /// Set to true if the pump encountered an error during subscription setup.
    pub start_failed: AtomicBool,
    /// Error message if subscription setup failed.
    pub start_error: parking_lot::Mutex<Option<String>>,
    /// One-shot channel to notify when subscription is started (or failed).
    pub start_notify_tx: mpsc::Sender<()>,
    /// One-shot channel receiver for start notification.
    pub start_notify_rx: parking_lot::Mutex<Option<mpsc::Receiver<()>>>,
}

impl SubscriptionHandle {
    /// Signal the pump that we are unsubscribing, and abort the task.
    pub fn unsubscribe(&self) {
        self.unsubscribed.store(true, Ordering::SeqCst);
        // Close the notify_rx so __anext__ returns None → StopAsyncIteration
        *self.notify_rx.lock() = None;
        // Abort the pump task
        let task_handle = self.task.lock().take();
        if let Some(handle) = task_handle {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Raw subscription items (stored in buffers, no Python conversion yet)
// ---------------------------------------------------------------------------

/// Raw items from subscription streams, before Python conversion.
#[derive(Debug)]
pub enum RawSubItem {
    /// A block header.
    Header(alloy::rpc::types::Header),
    /// A full block.
    Block(alloy::rpc::types::Block),
    /// A pending transaction hash.
    PendingTxHash(B256),
    /// A full pending transaction.
    FullPendingTx(parking_lot::Mutex<Option<serde_json::Value>>),
    /// A log.
    Log(alloy::rpc::types::Log),
    /// The subscription ended cleanly.
    End,
    /// The connection dropped or a transport error occurred.
    Disconnected { message: String },
}

// ---------------------------------------------------------------------------
// Pump task helpers
// ---------------------------------------------------------------------------

/// Write a raw item to the active buffer, set signal, and optionally send wake-up.
fn buffer_item(handle: &SubscriptionHandle, item: RawSubItem) {
    let active = handle.active.load(Ordering::Relaxed);
    handle.buffers[active].lock().push(item);

    // Coalesced signal: send wake-up only on false→true transition
    if !handle.signaled.swap(true, Ordering::Relaxed) {
        let _ = handle.notify_tx.try_send(());
    }
}

/// Signal that the subscription has successfully started.
fn signal_started(handle: &SubscriptionHandle) {
    handle.started.store(true, Ordering::Release);
    let _ = handle.start_notify_tx.try_send(());
}

/// Signal that the subscription failed to start.
fn signal_start_failed(handle: &SubscriptionHandle, message: String) {
    *handle.start_error.lock() = Some(message);
    handle.start_failed.store(true, Ordering::Release);
    let _ = handle.start_notify_tx.try_send(());
}

// ---------------------------------------------------------------------------
// Pump tasks for each subscription type
// ---------------------------------------------------------------------------

/// Pump task for block header subscriptions.
///
/// Drives a [`HeaderStream`] through [`pump_header_stream`] with a header
/// watchdog: a `newHeads` subscription that delivers no header within
/// [`HEADER_WATCHDOG_SECS`] is torn down and reconnected, reusing the provider's
/// existing backend reconnect path (`is_backend_gone` /
/// `is_pubsub_unavailable`) rather than a duplicate WS backoff. During reconnect
/// a [`RawSubItem::Disconnected`] is buffered so the Python side sees a clean
/// restart, not a silent gap (O3JW5S).
pub async fn pump_blocks(provider: Arc<dyn Provider<Ethereum>>, handle: Arc<SubscriptionHandle>) {
    let stream: HeaderStream = match provider.subscribe_blocks().await {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream().boxed()
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to blocks: {e}"));
            return;
        }
    };

    // `provider` is reused (not duplicated) by the reconnect closure: each
    // watchdog fire re-issues `subscribe_blocks` on the same provider, leaning
    // on alloy-pubsub's backend reconnect for a dead socket.
    let reconnect_provider = Arc::clone(&provider);
    pump_header_stream(
        handle,
        stream,
        Duration::from_secs(HEADER_WATCHDOG_SECS),
        move || {
            let provider = Arc::clone(&reconnect_provider);
            async move { reconnect_new_heads_stream(provider).await }
        },
    )
    .await;
}

/// Re-establish a `newHeads` [`HeaderStream`] after the watchdog tore down a
/// stalled subscription.
///
/// Reuses the provider's existing backend reconnect path: `subscribe_blocks`
/// re-issues `eth_subscribe`; if the WS backend is dead, alloy-pubsub's
/// `is_backend_gone` / `is_pubsub_unavailable` retry path reconnects the
/// transport (the same path that fires on request errors during normal RPC).
/// Each subscribe attempt is bounded by [`HEADER_WATCHDOG_SECS`] so a hung dead
/// socket (one that accepts the `eth_subscribe` write but never responds)
/// cannot re-stall the pump. Retries indefinitely with the shared backoff curve
/// ([`crate::provider::INITIAL_RETRY_DELAY_MS`] etc.) so a transiently-downed
/// provider recovers without an operator restart — the bot process stays alive.
async fn reconnect_new_heads_stream(provider: Arc<dyn Provider<Ethereum>>) -> Option<HeaderStream> {
    use crate::provider::{BACKOFF_MULTIPLIER, INITIAL_RETRY_DELAY_MS, MAX_RETRY_DELAY_MS};
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;
    loop {
        match timeout(
            Duration::from_secs(HEADER_WATCHDOG_SECS),
            provider.subscribe_blocks(),
        )
        .await
        {
            Ok(Ok(s)) => return Some(s.into_stream().boxed()),
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "degenbot_rpc::subscription",
                    %e,
                    delay_ms,
                    "newHeads reconnect subscribe failed — retrying"
                );
            }
            Err(_) => {
                tracing::warn!(
                    target: "degenbot_rpc::subscription",
                    delay_ms,
                    "newHeads reconnect subscribe hung — retrying"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        delay_ms = std::cmp::min(
            delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
            MAX_RETRY_DELAY_MS,
        );
    }
}

/// Drive a `newHeads` header stream with a header watchdog.
///
/// For each header from `stream`, buffer it via [`buffer_item`]. If no header
/// arrives within `watchdog`, the subscription is treated as a silently
/// half-open socket: a [`RawSubItem::Disconnected`] is buffered (so the Python
/// side sees a clean restart, not a silent gap), the dead stream is dropped,
/// and `next_stream` is called to re-establish the subscription.
///
/// Separated from [`pump_blocks`] so the watchdog/reconnect decision is unit-
/// testable with an injected never-yielding stream + a tiny threshold (no live
/// RPC — see `test_header_watchdog_fires_on_never_yielding_stream`).
async fn pump_header_stream<S, F, Fut>(
    handle: Arc<SubscriptionHandle>,
    mut stream: S,
    watchdog: Duration,
    mut next_stream: F,
) where
    S: futures_util::Stream<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Option<S>> + Send + 'static,
{
    loop {
        // Watchdog: bound `stream.next()` so a silently-stalled socket (no
        // close frame, never resolves) is torn down within a bounded window
        // instead of hanging the pump forever.
        match timeout(watchdog, stream.next()).await {
            Ok(Some(header)) => {
                buffer_item(&handle, RawSubItem::Header(header));
                if handle.unsubscribed.load(Ordering::Relaxed) {
                    return;
                }
            }
            Ok(None) => {
                // Clean subscription end (real close frame). Match the
                // pre-watchdog behavior: buffer End and stop.
                buffer_item(&handle, RawSubItem::End);
                return;
            }
            Err(_) => {
                // Watchdog fired: no header within `watchdog`. Buffer a clean
                // disconnect marker so Python sees a restart, drop the dead
                // stream, and re-subscribe via `next_stream`.
                buffer_item(
                    &handle,
                    RawSubItem::Disconnected {
                        message: "newHeads subscription stall: no header within watchdog window, reconnecting".to_string(),
                    },
                );
                match next_stream().await {
                    Some(s) => stream = s,
                    None => return,
                }
            }
        }
    }
}

/// Pump task for full block subscriptions.
pub async fn pump_full_blocks(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let sub_result = provider.subscribe_full_blocks().full().into_stream().await;
    let mut stream = match sub_result {
        Ok(s) => {
            signal_started(&handle);
            s
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to full blocks: {e}"));
            return;
        }
    };

    loop {
        match stream.next().await {
            Some(Ok(block)) => {
                buffer_item(&handle, RawSubItem::Block(block));
            }
            Some(Err(e)) => {
                buffer_item(
                    &handle,
                    RawSubItem::Disconnected {
                        message: format!("Full block subscription error: {e}"),
                    },
                );
                return;
            }
            None => {
                buffer_item(&handle, RawSubItem::End);
                return;
            }
        }

        if handle.unsubscribed.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Pump task for pending transaction hash subscriptions.
pub async fn pump_pending_transactions(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let sub_result = provider.subscribe_pending_transactions().await;
    let mut stream = match sub_result {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(
                &handle,
                format!("Failed to subscribe to pending transactions: {e}"),
            );
            return;
        }
    };

    loop {
        let Some(hash) = stream.next().await else {
            buffer_item(&handle, RawSubItem::End);
            return;
        };

        buffer_item(&handle, RawSubItem::PendingTxHash(hash));

        if handle.unsubscribed.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Pump task for full pending transaction subscriptions.
pub async fn pump_full_pending_transactions(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let sub_result = provider.subscribe_full_pending_transactions().await;
    let mut stream = match sub_result {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(
                &handle,
                format!("Failed to subscribe to full pending transactions: {e}"),
            );
            return;
        }
    };

    loop {
        let Some(tx_data) = stream.next().await else {
            buffer_item(&handle, RawSubItem::End);
            return;
        };

        // Serialize the transaction to JSON now (in the tokio task, no GIL)
        // and store as serde_json::Value. Python conversion happens in drain().
        let json_val = match serde_json::to_value(&tx_data) {
            Ok(v) => v,
            Err(e) => {
                buffer_item(
                    &handle,
                    RawSubItem::Disconnected {
                        message: format!("Transaction serialization error: {e}"),
                    },
                );
                return;
            }
        };
        buffer_item(
            &handle,
            RawSubItem::FullPendingTx(parking_lot::Mutex::new(Some(json_val))),
        );

        if handle.unsubscribed.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Pump task for log subscriptions.
pub async fn pump_logs(
    provider: Arc<dyn Provider<Ethereum>>,
    filter: Filter,
    handle: Arc<SubscriptionHandle>,
) {
    let sub_result = provider.subscribe_logs(&filter).await;
    let mut stream = match sub_result {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to logs: {e}"));
            return;
        }
    };

    loop {
        let Some(log) = stream.next().await else {
            buffer_item(&handle, RawSubItem::End);
            return;
        };

        buffer_item(&handle, RawSubItem::Log(log));

        if handle.unsubscribed.load(Ordering::Relaxed) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Bulk conversion: RawSubItem → Python objects (called with GIL held)
// ---------------------------------------------------------------------------

/// Result of draining the raw (pre-conversion) buffer.
#[derive(Debug)]
pub enum RawDrainResult {
    /// Normal items.
    Items(Vec<RawSubItem>),
    /// The subscription ended cleanly. Contains items before the end marker.
    Ended(Vec<RawSubItem>),
    /// The subscription disconnected. Contains items before the disconnect.
    Disconnected {
        items: Vec<RawSubItem>,
        message: String,
    },
}

impl SubscriptionHandle {
    /// Drain the stale buffer, returning raw items without `PyO3` conversion.
    /// Clears the signaled flag first, then swaps buffers.
    ///
    /// This is the pure-Rust core of [`drain_buffer`], separated so the
    /// double-buffer mechanics can be unit-tested without a Python interpreter.
    pub fn drain_raw(&self) -> RawDrainResult {
        // Clear signal BEFORE swapping — any new event arriving during drain
        // will see signaled=false and send a fresh wake-up
        self.signaled.store(false, Ordering::Relaxed);

        // Swap active buffer: what was active becomes stale (for us to drain),
        // what was stale becomes active (for pump to write to)
        let stale = self.active.fetch_xor(1, Ordering::AcqRel);

        // Take the stale buffer contents (leaves it empty for next cycle)
        let items = std::mem::take(&mut *self.buffers[stale].lock());

        // Scan for terminal markers (End / Disconnected)
        let mut before_marker = Vec::new();
        for item in items {
            match item {
                RawSubItem::End => return RawDrainResult::Ended(before_marker),
                RawSubItem::Disconnected { message } => {
                    return RawDrainResult::Disconnected {
                        items: before_marker,
                        message,
                    };
                }
                other => before_marker.push(other),
            }
        }

        RawDrainResult::Items(before_marker)
    }
}
// AlloyProvider subscription method implementations
// ---------------------------------------------------------------------------

/// Create a `SubscriptionHandle` with double-buffer infrastructure and spawn a pump task.
/// Returns an Arc<SubscriptionHandle> shared between the pump task and the Python side.
fn spawn_subscription_arc<F, Fut>(pump_fn: F) -> Arc<SubscriptionHandle>
where
    F: FnOnce(Arc<SubscriptionHandle>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (notify_tx, notify_rx) = mpsc::channel(16);
    let (start_notify_tx, start_notify_rx) = mpsc::channel(1);

    let handle = Arc::new(SubscriptionHandle {
        buffers: [
            parking_lot::Mutex::new(Vec::new()),
            parking_lot::Mutex::new(Vec::new()),
        ],
        active: AtomicUsize::new(0),
        signaled: AtomicBool::new(false),
        notify_tx,
        notify_rx: parking_lot::Mutex::new(Some(notify_rx)),
        task: parking_lot::Mutex::new(None),
        unsubscribed: AtomicBool::new(false),
        started: AtomicBool::new(false),
        start_failed: AtomicBool::new(false),
        start_error: parking_lot::Mutex::new(None),
        start_notify_tx,
        start_notify_rx: parking_lot::Mutex::new(Some(start_notify_rx)),
    });

    let task = get_runtime().spawn(pump_fn(Arc::clone(&handle)));

    *handle.task.lock() = Some(task);

    handle
}

impl AlloyProvider {
    /// Subscribe to new block headers.
    #[must_use]
    pub fn subscribe_blocks(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_blocks(provider, handle))
    }

    /// Subscribe to full block bodies (with full transactions).
    #[must_use]
    pub fn subscribe_full_blocks(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_full_blocks(provider, handle))
    }

    /// Subscribe to pending transaction hashes.
    #[must_use]
    pub fn subscribe_pending_transactions(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_pending_transactions(provider, handle))
    }

    /// Subscribe to full pending transaction bodies.
    #[must_use]
    pub fn subscribe_full_pending_transactions(&self) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_full_pending_transactions(provider, handle))
    }

    /// Subscribe to logs matching the given filter.
    #[must_use]
    pub fn subscribe_logs(&self, filter: Filter) -> Arc<SubscriptionHandle> {
        let provider = self.provider_arc();
        spawn_subscription_arc(move |handle| pump_logs(provider, filter, handle))
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[expect(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    /// Create a `SubscriptionHandle` with double-buffer infrastructure for testing.
    /// No pump task is spawned — tests push items directly into buffers.
    fn make_handle() -> Arc<SubscriptionHandle> {
        let (notify_tx, notify_rx) = mpsc::channel(16);
        let (start_notify_tx, start_notify_rx) = mpsc::channel(1);

        Arc::new(SubscriptionHandle {
            buffers: [
                parking_lot::Mutex::new(Vec::new()),
                parking_lot::Mutex::new(Vec::new()),
            ],
            active: AtomicUsize::new(0),
            signaled: AtomicBool::new(false),
            notify_tx,
            notify_rx: parking_lot::Mutex::new(Some(notify_rx)),
            task: parking_lot::Mutex::new(None),
            unsubscribed: AtomicBool::new(false),
            started: AtomicBool::new(true),
            start_failed: AtomicBool::new(false),
            start_error: parking_lot::Mutex::new(None),
            start_notify_tx,
            start_notify_rx: parking_lot::Mutex::new(Some(start_notify_rx)),
        })
    }

    #[test]
    fn test_drain_raw_empty_buffer_returns_items_empty() {
        let handle = make_handle();
        let result = handle.drain_raw();
        match result {
            RawDrainResult::Items(items) => assert!(items.is_empty()),
            _ => panic!("Expected Items variant with empty vec"),
        }
    }

    #[test]
    fn test_drain_raw_header_returns_items() {
        let handle = make_handle();

        // Push a header into buffer[0] (the active buffer)
        let header = alloy::rpc::types::Header::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Items(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], RawSubItem::Header(_)));
            }
            _ => panic!("Expected Items variant"),
        }
    }

    #[test]
    fn test_drain_raw_log_returns_items() {
        let handle = make_handle();

        let log = alloy::rpc::types::Log::default();
        handle.buffers[0].lock().push(RawSubItem::Log(log));

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Items(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], RawSubItem::Log(_)));
            }
            _ => panic!("Expected Items variant"),
        }
    }

    #[test]
    fn test_drain_raw_end_marker_returns_ended() {
        let handle = make_handle();

        // Push a header then an End marker
        let header = alloy::rpc::types::Header::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));
        handle.buffers[0].lock().push(RawSubItem::End);

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Ended(items) => {
                // Only the header; End is consumed by drain_raw
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], RawSubItem::Header(_)));
            }
            _ => panic!("Expected Ended variant"),
        }
    }

    #[test]
    fn test_drain_raw_disconnected_returns_disconnected() {
        let handle = make_handle();

        // Push a header then a Disconnected marker
        let header = alloy::rpc::types::Header::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));
        handle.buffers[0].lock().push(RawSubItem::Disconnected {
            message: "connection lost".to_string(),
        });

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Disconnected { items, message } => {
                assert_eq!(items.len(), 1);
                assert_eq!(message, "connection lost");
            }
            _ => panic!("Expected Disconnected variant"),
        }
    }

    #[test]
    fn test_drain_raw_double_buffer_swap_isolates_reads_from_writes() {
        let handle = make_handle();

        // Write to buffer[0] (active=0)
        let header1 = alloy::rpc::types::Header::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header1));

        // First drain: swaps active to 1, drains buffer[0]
        let result1 = handle.drain_raw();
        match result1 {
            RawDrainResult::Items(items) => assert_eq!(items.len(), 1),
            _ => panic!("Expected Items"),
        }
        assert_eq!(handle.active.load(Ordering::Relaxed), 1);

        // Write to buffer[1] (now active=1)
        let header2 = alloy::rpc::types::Header::default();
        handle.buffers[1].lock().push(RawSubItem::Header(header2));

        // Second drain: swaps active to 0, drains buffer[1]
        let result2 = handle.drain_raw();
        match result2 {
            RawDrainResult::Items(items) => assert_eq!(items.len(), 1),
            _ => panic!("Expected Items"),
        }
        assert_eq!(handle.active.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_drain_raw_clears_signaled_flag() {
        let handle = make_handle();
        handle.signaled.store(true, Ordering::Relaxed);

        let _ = handle.drain_raw();

        assert!(!handle.signaled.load(Ordering::Relaxed));
    }

    #[test]
    fn test_drain_raw_buffer_is_empty_after_drain() {
        let handle = make_handle();

        let header = alloy::rpc::types::Header::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));

        let _ = handle.drain_raw();

        // The drained buffer (was 0, now stale after swap) must be empty
        assert!(handle.buffers[0].lock().is_empty());
    }

    #[test]
    fn test_drain_raw_multiple_items_before_end() {
        let handle = make_handle();

        let header = alloy::rpc::types::Header::default();
        let log = alloy::rpc::types::Log::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));
        handle.buffers[0].lock().push(RawSubItem::Log(log));
        handle.buffers[0].lock().push(RawSubItem::End);

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Ended(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], RawSubItem::Header(_)));
                assert!(matches!(items[1], RawSubItem::Log(_)));
            }
            _ => panic!("Expected Ended variant"),
        }
    }

    #[test]
    fn test_drain_raw_disconnected_ignores_trailing_items() {
        let handle = make_handle();

        let header = alloy::rpc::types::Header::default();
        let log = alloy::rpc::types::Log::default();
        handle.buffers[0].lock().push(RawSubItem::Header(header));
        handle.buffers[0].lock().push(RawSubItem::Disconnected {
            message: "fail".to_string(),
        });
        // Items after Disconnected are unreachable — drain stops at the marker
        handle.buffers[0].lock().push(RawSubItem::Log(log));

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Disconnected { items, message } => {
                assert_eq!(items.len(), 1); // Only the header before the marker
                assert_eq!(message, "fail");
            }
            _ => panic!("Expected Disconnected variant"),
        }
    }

    /// A `newHeads` stream that NEVER yields (silently half-open socket) must
    /// trip the watchdog within the threshold and buffer a clean
    /// `Disconnected` — not hang the pump forever (O3JW5S).
    #[tokio::test]
    async fn test_header_watchdog_fires_on_never_yielding_stream() {
        use futures_util::stream;

        let handle = make_handle();
        // A header-shaped stream that NEVER yields — simulates a silently
        // half-open WS socket where `stream.next()` never resolves.
        let stalled: stream::Pending<alloy::rpc::types::Header> = stream::pending();
        // Reconnect returns None → the pump exits after the first watchdog
        // fire (we are testing the watchdog decision, not the reconnect loop).
        let start = tokio::time::Instant::now();

        pump_header_stream(
            Arc::clone(&handle),
            stalled,
            Duration::from_millis(20),
            || async move { None },
        )
        .await;

        let elapsed = start.elapsed();
        // The watchdog must fire within a bounded window — well below the
        // ~48s production threshold, and not hang.
        assert!(
            elapsed < Duration::from_secs(2),
            "watchdog did not fire within bounded time (elapsed={elapsed:?})"
        );

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Disconnected { items, message } => {
                assert!(items.is_empty(), "no headers buffered before the stall");
                assert!(
                    message.contains("newHeads subscription stall"),
                    "disconnect message must carry the greppable stall prefix: {message}"
                );
            }
            other => panic!("Expected Disconnected after watchdog, got {other:?}"),
        }
    }

    /// A healthy stream (headers arrive before the watchdog) must NOT be torn
    /// down — the watchdog only fires on silence, not on normal flow.
    #[tokio::test]
    async fn test_header_watchdog_does_not_fire_on_healthy_stream() {
        use futures_util::stream;

        let handle = make_handle();
        // Two real headers then a clean end — no gap exceeds the watchdog.
        let headers = vec![
            alloy::rpc::types::Header::default(),
            alloy::rpc::types::Header::default(),
        ];
        let s = stream::iter(headers);

        pump_header_stream(
            Arc::clone(&handle),
            s,
            Duration::from_mins(1),
            || async move { None },
        )
        .await;

        let result = handle.drain_raw();
        match result {
            RawDrainResult::Ended(items) => {
                assert_eq!(items.len(), 2, "both headers buffered, then clean End");
            }
            other => {
                panic!("Expected clean Ended (no watchdog fire) on healthy stream, got {other:?}")
            }
        }
    }
}
