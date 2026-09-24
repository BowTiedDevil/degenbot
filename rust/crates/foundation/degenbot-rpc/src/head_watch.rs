//! Subscription-driven head watch.
//!
//! [`HeadWatch`] is the transport half of the process hub's head source: it
//! performs the `newHeads` subscribe + reconnect (reusing the FFI subscription
//! pump's watchdog machinery), and publishes every observed header into the
//! hub's `NewHead` / `LatestOnly` channel. The hub owns the latest head and
//! the staleness clock ([`degenbot_eventhub::HeadSubscription`]); a consumer
//! awaits [`degenbot_eventhub::HeadSubscription::changed`] instead of polling
//! `eth_blockNumber` on a timer.
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
use degenbot_eventhub::head::HeadSender;
use degenbot_eventhub::{Hub, HubEvent};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for a [`HeadWatch`].
#[derive(Debug, Clone, Copy)]
pub struct HeadWatchConfig {
    /// Tear down + reconnect a subscription that delivers no header for this
    /// long. Defaults to [`HEADER_WATCHDOG_SECS`], matching the FFI pump.
    pub watchdog: Duration,
}

impl Default for HeadWatchConfig {
    fn default() -> Self {
        Self {
            watchdog: Duration::from_secs(HEADER_WATCHDOG_SECS),
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
    /// The hub already had a `NewHead` source registered.
    #[error("head source registration failed: {0}")]
    Register(#[from] degenbot_eventhub::HubError),
}

/// A live `newHeads` subscription feeding the hub head source.
///
/// The driving task owns the subscription and publishes every observed header
/// into the hub; the hub owns the latest head and the staleness clock.
/// Dropping this handle does not stop the task: the reconnect watchdog keeps
/// the transport alive for the process lifetime, so a later reader sees the
/// current head rather than a stale one.
#[derive(Debug)]
pub struct HeadWatch;

impl HeadWatch {
    /// Subscribe to `newHeads`, register the hub's `NewHead` source, and start
    /// publishing headers into it.
    ///
    /// The initial subscribe is awaited here so a failure is reported
    /// synchronously and the caller can fall back to polling; the hub source
    /// is registered only after that subscribe succeeds. Once complete, the
    /// driving task reconnects in the background on the shared curve.
    ///
    /// # Errors
    ///
    /// [`HeadWatchError::Subscribe`] if the initial `subscribe_blocks` fails
    /// or does not answer within `config.watchdog`;
    /// [`HeadWatchError::Register`] if the hub already has a `NewHead` source.
    pub async fn subscribe(
        hub: &Hub,
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

        let sender: HeadSender = hub.register_head_source()?;
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
                    tracing::debug!(number = header.number, "head watch header");
                    sender.publish(HubEvent::NewHead {
                        number: header.number,
                        timestamp: header.timestamp,
                        base_fee_per_gas: header.base_fee_per_gas,
                        gas_used: header.gas_used,
                        gas_limit: header.gas_limit,
                    });
                    true
                },
                move || {
                    let n = stall_attempt.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(attempt = n, "head watch reconnect attempt");
                    true
                },
                move || {
                    let n = end_attempt.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(attempt = n, "head watch reconnect attempt");
                    true
                },
            )
            .await;
        });

        Ok(Self)
    }
}
