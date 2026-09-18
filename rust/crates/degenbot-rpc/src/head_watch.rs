//! Subscription-driven head watch.
//!
//! A consumer that must react to new blocks (the backrun sidecar's head
//! advance) otherwise has to poll `eth_blockNumber` on a timer: an RPC
//! round-trip per tick that still cannot fire the instant a head lands.
//! [`HeadWatch`] turns a `newHeads` subscription into a
//! [`tokio::sync::watch`] channel carrying the head block NUMBER, so a
//! consumer awaits [`HeadWatch::head_rx`]`().changed()` and reads the current
//! value without awaiting.
//!
//! It reuses the FFI subscription pump's watchdog + reconnect machinery
//! ([`crate::subscription::drive_new_heads`] /
//! [`crate::subscription::reconnect_new_heads_stream`]) rather than carrying a
//! second copy of the backoff curve: a silently half-open socket is torn down
//! within the watchdog and re-subscribed on the shared curve.

use crate::subscription::{
    drive_new_heads, reconnect_new_heads_stream, HeaderStream, HEADER_WATCHDOG_SECS,
};
use alloy::network::Ethereum;
use alloy::providers::Provider;
use degenbot_core::runtime::get_runtime;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Configuration for a [`HeadWatch`].
#[derive(Debug, Clone, Copy)]
pub struct HeadWatchConfig {
    /// Tear down + reconnect a subscription that delivers no header for this
    /// long. Defaults to [`HEADER_WATCHDOG_SECS`], matching the FFI pump.
    pub watchdog: Duration,
    /// Value the watch channel starts at, before the first header arrives. A
    /// consumer that already knows the head (e.g. from an initial
    /// `eth_blockNumber`) seeds it so the handoff from polling stays
    /// monotonic.
    pub initial_number: u64,
}

impl Default for HeadWatchConfig {
    fn default() -> Self {
        Self {
            watchdog: Duration::from_secs(HEADER_WATCHDOG_SECS),
            initial_number: 0,
        }
    }
}

/// Why a [`HeadWatch`] could not be created.
#[derive(Debug, thiserror::Error)]
pub enum HeadWatchError {
    /// The initial `eth_subscribe`(`newHeads`) failed, or did not answer
    /// within the watchdog window.
    #[error("head watch subscribe failed: {0}")]
    Subscribe(String),
}

/// A live `newHeads` subscription publishing the head block NUMBER.
///
/// The driving task owns the subscription and updates the watch on every
/// header. Dropping this handle does not stop the task: the latest head value
/// and the health probe keep advancing, so a later reader sees the current
/// head rather than a stale one.
#[derive(Debug)]
pub struct HeadWatch {
    head_rx: watch::Receiver<u64>,
    last_header_at: Arc<parking_lot::Mutex<Option<Instant>>>,
}

impl HeadWatch {
    /// Subscribe to `newHeads` and start publishing the head block number.
    ///
    /// The initial subscribe is awaited here so a failure is reported
    /// synchronously and the caller can fall back to polling; once it succeeds
    /// the driving task reconnects in the background on the shared curve.
    ///
    /// # Errors
    ///
    /// Returns [`HeadWatchError::Subscribe`] if the initial `subscribe_blocks`
    /// fails or does not answer within `config.watchdog`.
    pub async fn subscribe(
        provider: Arc<dyn Provider<Ethereum>>,
        config: HeadWatchConfig,
    ) -> Result<Self, HeadWatchError> {
        let watchdog = config.watchdog;
        let stream: HeaderStream =
            match tokio::time::timeout(watchdog, provider.subscribe_blocks()).await {
                Ok(Ok(s)) => s.into_stream().boxed(),
                Ok(Err(e)) => return Err(HeadWatchError::Subscribe(format!("{e}"))),
                Err(_) => {
                    return Err(HeadWatchError::Subscribe(format!(
                        "eth_subscribe(newHeads) did not answer within {}s",
                        watchdog.as_secs()
                    )))
                }
            };
        tracing::info!("head watch subscribed");

        let (tx, head_rx) = watch::channel(config.initial_number);
        let last_header_at = Arc::new(parking_lot::Mutex::new(Some(Instant::now())));
        let last_header_at_task = Arc::clone(&last_header_at);
        let provider_for_reconnect = Arc::clone(&provider);
        let attempt = Arc::new(AtomicU32::new(0));
        let stall_attempt = Arc::clone(&attempt);
        let end_attempt = Arc::clone(&attempt);

        get_runtime().spawn(async move {
            let reconnect = move || {
                let provider = Arc::clone(&provider_for_reconnect);
                async move { reconnect_new_heads_stream(provider).await }
            };
            drive_new_heads(
                stream,
                watchdog,
                reconnect,
                move |header| {
                    *last_header_at_task.lock() = Some(Instant::now());
                    tracing::debug!(number = header.number, "head watch header");
                    // Replace (not `send`): a momentarily subscriber-less
                    // watch must still advance its value for the next reader.
                    tx.send_replace(header.number);
                    true
                },
                move || {
                    let n = stall_attempt.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(attempt = n, "head watch reconnect attempt");
                    true
                },
                move || {
                    // A clean close is still a lost subscription; reconnect
                    // rather than silently parking the watch.
                    let n = end_attempt.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(attempt = n, "head watch reconnect attempt");
                    true
                },
            )
            .await;
        });

        Ok(Self {
            head_rx,
            last_header_at,
        })
    }

    /// A receiver for the head block number. `changed()` resolves on each new
    /// header; `borrow()` reads the current value without awaiting.
    #[must_use]
    pub fn head_rx(&self) -> watch::Receiver<u64> {
        self.head_rx.clone()
    }

    /// Whether no header has arrived for at least `threshold`. Initialized to
    /// `false` at subscribe, so a just-created watch is not immediately stale.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        match *self.last_header_at.lock() {
            Some(at) => at.elapsed() > threshold,
            None => true,
        }
    }

    /// Time since the last header, or `None` if the watch never observed one.
    #[must_use]
    pub fn last_header_age(&self) -> Option<Duration> {
        self.last_header_at.lock().map(|at| at.elapsed())
    }
}
