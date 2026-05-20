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
use crate::py_converters::{block_to_py_dict, header_to_py_dict, log_to_py_dict};
use crate::runtime::get_runtime;
use alloy::network::Ethereum;
use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use futures_util::StreamExt;
use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

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
pub(crate) async fn pump_blocks(
    provider: Arc<dyn Provider<Ethereum>>,
    handle: Arc<SubscriptionHandle>,
) {
    let sub_result = provider.subscribe_blocks().await;
    let mut stream = match sub_result {
        Ok(s) => {
            signal_started(&handle);
            s.into_stream()
        }
        Err(e) => {
            signal_start_failed(&handle, format!("Failed to subscribe to blocks: {e}"));
            return;
        }
    };

    loop {
        let Some(header) = stream.next().await else {
            buffer_item(&handle, RawSubItem::End);
            return;
        };

        buffer_item(&handle, RawSubItem::Header(header));

        if handle.unsubscribed.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Pump task for full block subscriptions.
pub(crate) async fn pump_full_blocks(
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
pub(crate) async fn pump_pending_transactions(
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
pub(crate) async fn pump_full_pending_transactions(
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
pub(crate) async fn pump_logs(
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
pub(crate) enum RawDrainResult {
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

/// Result of draining a buffer and converting to Python objects.
pub(crate) enum DrainResult {
    /// Normal items converted to Python objects.
    Items(Vec<Py<PyAny>>),
    /// The subscription ended cleanly. Contains any items before the end marker.
    Ended(Vec<Py<PyAny>>),
    /// The subscription disconnected. Contains any items before the disconnect.
    Disconnected {
        items: Vec<Py<PyAny>>,
        message: String,
    },
}

/// Convert a single raw item to a Python object. Returns None for End/Disconnected
/// (these are handled by `DrainResult` variants).
fn convert_item(py: Python<'_>, item: RawSubItem) -> PyResult<Option<Py<PyAny>>> {
    match item {
        RawSubItem::Header(header) => {
            let d = header_to_py_dict(py, &header)?;
            Ok(Some(d.into_any().unbind()))
        }
        RawSubItem::Block(block) => {
            let d = block_to_py_dict(py, &block)?;
            Ok(Some(d.into_any().unbind()))
        }
        RawSubItem::PendingTxHash(hash) => {
            let hex_str = hash.to_string();
            Ok(Some(
                pyo3::types::PyString::new(py, &hex_str)
                    .into_any()
                    .unbind(),
            ))
        }
        RawSubItem::FullPendingTx(json_guard) => {
            let json_val = json_guard
                .lock()
                .take()
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("Transaction data already consumed"))?;
            let obj = crate::py_converters::json_to_py_with_hexbytes(py, json_val)?;
            Ok(Some(obj.unbind()))
        }
        RawSubItem::Log(log) => {
            let d = log_to_py_dict(py, &log)?;
            Ok(Some(d.into_any().unbind()))
        }
        RawSubItem::End | RawSubItem::Disconnected { message: _ } => Ok(None),
    }
}

impl SubscriptionHandle {
    /// Drain the stale buffer, returning raw items without `PyO3` conversion.
    /// Clears the signaled flag first, then swaps buffers.
    ///
    /// This is the pure-Rust core of [`drain_buffer`], separated so the
    /// double-buffer mechanics can be unit-tested without a Python interpreter.
    pub(crate) fn drain_raw(&self) -> RawDrainResult {
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

/// Drain the stale buffer and convert all items to Python objects.
/// Called with the GIL held. Delegates buffer mechanics to [`SubscriptionHandle::drain_raw`].
pub(crate) fn drain_buffer(handle: &SubscriptionHandle, py: Python<'_>) -> PyResult<DrainResult> {
    let raw = handle.drain_raw();

    // Convert raw items to Python objects
    let convert_all = |items: Vec<RawSubItem>| -> PyResult<Vec<Py<PyAny>>> {
        let mut converted = Vec::with_capacity(items.len());
        for item in items {
            if let Some(py_obj) = convert_item(py, item)? {
                converted.push(py_obj);
            }
        }
        Ok(converted)
    };

    match raw {
        RawDrainResult::Items(items) => Ok(DrainResult::Items(convert_all(items)?)),
        RawDrainResult::Ended(items) => Ok(DrainResult::Ended(convert_all(items)?)),
        RawDrainResult::Disconnected { items, message } => Ok(DrainResult::Disconnected {
            items: convert_all(items)?,
            message,
        }),
    }
}

// ---------------------------------------------------------------------------
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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
}
