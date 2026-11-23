//! Subscription-driven head watch.
//!
//! [`HeadWatch`] is the transport half of the process hub's head clock: it
//! performs the `newHeads` subscribe + reconnect (reusing the FFI subscription
//! pump's watchdog machinery), and publishes every observed header through the
//! hub's claimed [`HeadSender`]. The hub owns the latest head and the staleness
//! clock ([`degenbot_eventhub::HeadSubscription`]); a consumer awaits
//! [`degenbot_eventhub::HeadSubscription::changed`] instead of polling
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
use degenbot_core::canonical_head::{ChainAnchor, ChainAnchorError};
use degenbot_eventhub::{Head, HeadMetadata, HeadSender};
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

/// The exact head one observed `newHeads` header states: its own hash and its
/// parent hash are the header's, the chain is the session's, and the height is
/// the header's. Pure: the header already carried every identity field, so a
/// full-header observation spends no request on identity.
///
/// # Errors
///
/// [`ChainAnchorError`] when the header names no block (a zero hash or zero
/// parent hash) — a partial identity is refused, never published with a
/// zero-filled field.
fn anchored_head(
    chain_id: u64,
    header: &alloy::rpc::types::Header,
) -> Result<Head, ChainAnchorError> {
    let anchor = ChainAnchor::new(chain_id, header.number, header.hash, header.parent_hash)?;
    Ok(Head::new(
        anchor,
        Some(HeadMetadata {
            timestamp: header.timestamp,
            base_fee_per_gas: header.base_fee_per_gas,
            gas_used: header.gas_used,
            gas_limit: header.gas_limit,
        }),
    ))
}

/// Why a [`HeadWatch`] could not be created.
#[derive(Debug, thiserror::Error)]
pub enum HeadWatchError {
    /// The initial `eth_subscribe`(`newHeads`) failed, or did not answer
    /// within the watchdog window.
    #[error("head watch subscribe failed: {0}")]
    Subscribe(String),
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
    /// Subscribe to `newHeads` and start publishing headers through the hub's
    /// claimed head source.
    ///
    /// `sender` is the hub's one claim ([`degenbot_eventhub::Hub::claim_head_source`]),
    /// handed in by whichever boot hosts this watch: the watch never claims a
    /// source of its own. `chain_id` is the session's chain: a `newHeads`
    /// header carries its own hash and parent hash but never the chain it
    /// belongs to, so the boot states it once and every observed header becomes
    /// one exact [`degenbot_eventhub::ChainAnchor`] with no identity read. The initial subscribe is awaited here so a failure is
    /// reported synchronously and the caller can fall back to polling. Once
    /// complete, the driving task reconnects in the background on the shared
    /// curve.
    ///
    /// `spawn_on` is the runtime the driving task is spawned on. It MUST be the
    /// runtime the provider's pubsub service lives on, so a teardown that
    /// discards that runtime also cancels the driving task deterministically
    /// (the `AnvilFork` transport-session discipline: one runtime per
    /// transport, retired by one close). A caller hosting the provider on the
    /// ambient shared runtime passes
    /// `degenbot_core::runtime::get_runtime().handle().clone()`.
    ///
    /// # Errors
    ///
    /// [`HeadWatchError::Subscribe`] if the initial `subscribe_blocks` fails
    /// or does not answer within `config.watchdog`.
    pub async fn subscribe(
        sender: HeadSender,
        provider: Arc<dyn Provider<Ethereum>>,
        chain_id: u64,
        config: HeadWatchConfig,
        spawn_on: tokio::runtime::Handle,
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

        let provider_for_reconnect = Arc::clone(&provider);
        let attempt = Arc::new(AtomicU32::new(0));
        let stall_attempt = Arc::clone(&attempt);
        let end_attempt = Arc::clone(&attempt);

        spawn_on.spawn(async move {
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
                    match anchored_head(chain_id, &header) {
                        Ok(head) => sender.publish(head),
                        Err(error) => tracing::error!(
                            number = header.number,
                            %error,
                            "head watch header names no exact chain identity; head not published"
                        ),
                    }
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the test builds fixture headers and unwraps its own assertions"
)]
mod tests {
    use super::*;
    use alloy::primitives::B256;

    fn header(number: u64, hash: u8, parent: u8) -> alloy::rpc::types::Header {
        alloy::rpc::types::Header {
            hash: B256::with_last_byte(hash),
            inner: alloy::consensus::Header {
                number,
                parent_hash: B256::with_last_byte(parent),
                timestamp: 7,
                base_fee_per_gas: Some(9),
                gas_used: 11,
                gas_limit: 13,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_full_newheads_header_yields_its_exact_anchor_with_no_identity_read() {
        let observed = header(101, 0xaa, 0xa0);
        let head = anchored_head(1, &observed).expect("the header names its block");
        assert_eq!(
            head.anchor(),
            &ChainAnchor::new(
                1,
                101,
                B256::with_last_byte(0xaa),
                B256::with_last_byte(0xa0)
            )
            .unwrap(),
            "the anchor is the header's own identity, on the session's chain"
        );
        assert_eq!(head.number(), 101);
        let metadata = head.metadata().expect("a full header carries its facts");
        assert_eq!(metadata.timestamp, 7);
        assert_eq!(metadata.base_fee_per_gas, Some(9));
        assert_eq!(metadata.gas_used, 11);
        assert_eq!(metadata.gas_limit, 13);
    }

    #[test]
    fn a_header_with_no_block_hash_is_refused_not_zero_filled() {
        let mut observed = header(101, 0xaa, 0xa0);
        observed.hash = B256::ZERO;
        assert_eq!(
            anchored_head(1, &observed),
            Err(ChainAnchorError::MissingBlockHash),
            "an unnamed block is an ingress refusal"
        );
        let mut unparented = header(101, 0xaa, 0x00);
        unparented.inner.parent_hash = B256::ZERO;
        assert_eq!(
            anchored_head(1, &unparented),
            Err(ChainAnchorError::MissingParentHash)
        );
    }
}
