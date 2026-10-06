//! The typed pending-tx stream a hosted driver drains, and the pump its host
//! keeps.
//!
//! One hub class per source kind ([`PendingTxSource`]): the `MEVBlocker`
//! searcher feed and the chain-node txpool feed register — and are drained —
//! independently, so each arm's frame corpus is exactly its own feed's and a
//! second arm can run in the same process without a registration tombstone.
//!
//! One provisioning act, two views: [`PendingTxPump`] is the host-side handle
//! (feed telemetry, shutdown), [`PendingTxStream`] is the driver-side typed
//! drain. The pump's own ring view is never drained, so the driver's stream
//! sees every frame.

use degenbot_core::op_warn;
#[cfg(any(feature = "test-utils", test))]
use degenbot_eventhub::DropOldestSender;
use degenbot_eventhub::{
    DropOldestReceiver, Hub, HubClass, HubError, HubEvent, OverflowPolicy, PendingTx,
    PendingTxSource, SourceHandle, Subscription,
};

use crate::backrun_feed::{BackrunFeed, BackrunFeedConfig, BackrunFeedStatus, DROPPED_RING_METRIC};
use crate::txpool_feed::{TxpoolFeed, TxpoolFeedConfig, TXPOOL_DROPPED_RING_METRIC};

/// The per-kind feed configuration a host mints a pump from.
#[derive(Debug, Clone)]
pub enum PendingTxFeedConfig {
    /// The `MEVBlocker` searcher feed.
    Mevblocker(BackrunFeedConfig),
    /// The chain-node txpool feed.
    Txpool(TxpoolFeedConfig),
}

/// The host-side pump handle over one source kind's feed.
///
/// The hosted-sources owner keeps this for the feed-telemetry sampler and
/// shutdown; the frames flow to the [`PendingTxStream`] handed out at mint.
pub struct PendingTxPump {
    source: PendingTxSource,
    /// The ring's source end, kept for the test-support frame injection only
    /// (the live feed owns the real publishing path).
    #[cfg(any(feature = "test-utils", test))]
    sender: DropOldestSender,
    inner: PumpInner,
}

enum PumpInner {
    Mevblocker(BackrunFeed),
    Txpool(TxpoolFeed),
}

/// The driver-side typed drain over one source kind's hub ring.
///
/// The ring only ever carries [`HubEvent::PendingTx`] frames of this stream's
/// kind; a stray other variant is warned about (never silently discarded).
#[derive(Clone)]
pub struct PendingTxStream {
    source: PendingTxSource,
    ring: DropOldestReceiver,
}

impl PendingTxPump {
    /// Spawn the pump for `source` over the drop-oldest ring the host
    /// registered for that kind's [`HubClass::PendingTx`] class, and return
    /// the driver-side [`PendingTxStream`] subscribed to the same ring.
    ///
    /// The class must be registered first (`Hub::register_source` with
    /// [`OverflowPolicy::DropOldestCounted`]); the mint fails closed on a
    /// mismatch so a miswired host cannot attach a feed to another kind's
    /// ring.
    ///
    /// Must be called within a tokio runtime context, or the crate-global
    /// `degenbot_core::runtime::get_runtime()` is used.
    ///
    /// # Errors
    ///
    /// Propagates [`HubError`] when `cfg` does not match `source`, when the
    /// declared policy handle mismatches, or when the class is not
    /// registered.
    pub fn spawn_on_ring(
        hub: &Hub,
        source: PendingTxSource,
        cfg: PendingTxFeedConfig,
    ) -> Result<(Self, PendingTxStream), HubError> {
        let class = match (&cfg, source) {
            (PendingTxFeedConfig::Mevblocker(_), PendingTxSource::Mevblocker) => {
                HubClass::PendingTx(PendingTxSource::Mevblocker)
            }
            (PendingTxFeedConfig::Txpool(_), PendingTxSource::Txpool) => {
                HubClass::PendingTx(PendingTxSource::Txpool)
            }
            _ => {
                return Err(HubError::PolicyMismatch {
                    expected: "the feed config matching the source kind",
                })
            }
        };
        let SourceHandle::DropOldestCounted(sender) = hub.register_source(
            class,
            OverflowPolicy::DropOldestCounted {
                name: ring_metric(source),
            },
            ring_capacity(&cfg),
        )?
        else {
            return Err(HubError::PolicyMismatch {
                expected: "DropOldestCounted",
            });
        };
        let Subscription::DropOldestCounted(ring) = hub.subscribe(class)? else {
            return Err(HubError::PolicyMismatch {
                expected: "DropOldestCounted",
            });
        };
        let Subscription::DropOldestCounted(pump_ring) = hub.subscribe(class)? else {
            return Err(HubError::PolicyMismatch {
                expected: "DropOldestCounted",
            });
        };
        #[cfg(any(feature = "test-utils", test))]
        let pump_sender = sender.clone();
        let inner = match (source, cfg) {
            (PendingTxSource::Mevblocker, PendingTxFeedConfig::Mevblocker(c)) => {
                PumpInner::Mevblocker(BackrunFeed::build(c, sender.clone(), pump_ring))
            }
            (PendingTxSource::Txpool, PendingTxFeedConfig::Txpool(c)) => {
                PumpInner::Txpool(TxpoolFeed::build(c, sender.clone(), pump_ring))
            }
            _ => {
                return Err(HubError::PolicyMismatch {
                    expected: "the feed config matching the source kind",
                })
            }
        };
        #[cfg(any(feature = "test-utils", test))]
        let pump = Self {
            source,
            sender: pump_sender,
            inner,
        };
        #[cfg(not(any(feature = "test-utils", test)))]
        let pump = Self { source, inner };
        Ok((pump, PendingTxStream { source, ring }))
    }

    /// The source kind this pump provisions.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        self.source
    }

    /// The sampler snapshot (the `BackrunFeedStatus` field set both pumps
    /// share; the txpool feed's extra `mine_misses` counter is pump-scoped
    /// forensics, not an engine instrument).
    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        match &self.inner {
            PumpInner::Mevblocker(feed) => feed.status(),
            PumpInner::Txpool(feed) => {
                let st = feed.status();
                BackrunFeedStatus {
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
    }

    /// Stop the pump's transport. The ring (and the driver's stream) stay
    /// readable; only the reconnect loop ends.
    pub fn stop(&self) {
        match &self.inner {
            PumpInner::Mevblocker(feed) => feed.stop(),
            PumpInner::Txpool(feed) => feed.stop(),
        }
    }

    /// Test support: inject a frame into this pump's ring as its feed would.
    #[cfg(any(feature = "test-utils", test))]
    pub fn push(&self, event: HubEvent) {
        self.sender.push(event);
    }
}

fn ring_metric(source: PendingTxSource) -> &'static str {
    match source {
        PendingTxSource::Mevblocker => DROPPED_RING_METRIC,
        PendingTxSource::Txpool => TXPOOL_DROPPED_RING_METRIC,
    }
}

fn ring_capacity(cfg: &PendingTxFeedConfig) -> usize {
    match cfg {
        PendingTxFeedConfig::Mevblocker(c) => c.ring_capacity,
        PendingTxFeedConfig::Txpool(c) => c.ring_capacity,
    }
}

impl PendingTxStream {
    /// The source kind this stream drains.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        self.source
    }

    /// All buffered frames, ordered, atomically (rings swap; concurrent
    /// drains may interleave but never duplicate or lose between both
    /// buffers).
    #[must_use]
    pub fn drain(&self) -> Vec<PendingTx> {
        self.ring
            .drain()
            .into_iter()
            .filter_map(|event| match event {
                HubEvent::PendingTx(tx) => Some(tx),
                other => {
                    op_warn!(
                        domain = rpc,
                        event = ?other,
                        "pending stream: non-pending event on the pending ring"
                    );
                    None
                }
            })
            .collect()
    }

    /// Evictions counted on this stream's ring.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.ring.dropped()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "unit tests assert mint and drain outcomes"
)]
mod tests {
    use super::*;
    use crate::backrun_feed::BackrunFeedConfig;
    use crate::txpool_feed::TxpoolFeedConfig;

    fn frame(nonce: u64) -> HubEvent {
        HubEvent::PendingTx(PendingTx {
            raw_signed_tx: None,
            chain_id: 1,
            from: alloy::primitives::Address::ZERO,
            to: None,
            value: alloy::primitives::U256::ZERO,
            data: alloy::primitives::Bytes::new(),
            gas: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
            hash: alloy::primitives::B256::ZERO,
            access_list: serde_json::Value::Null,
            tx_type: 2,
            received_unix_ms: 0,
        })
    }

    /// Both source kinds mint on ONE hub: neither registration tombstones
    /// the other, and each stream's corpus is exactly its own feed's.
    #[test]
    fn both_kinds_mint_on_one_hub_and_drain_their_own_corpus() {
        let hub = Hub::new();
        let (mev_pump, mev_stream) = PendingTxPump::spawn_on_ring(
            &hub,
            PendingTxSource::Mevblocker,
            PendingTxFeedConfig::Mevblocker(BackrunFeedConfig::for_mainnet()),
        )
        .expect("the mevblocker kind registers");
        let (tx_pump, tx_stream) = PendingTxPump::spawn_on_ring(
            &hub,
            PendingTxSource::Txpool,
            PendingTxFeedConfig::Txpool(TxpoolFeedConfig::defaults()),
        )
        .expect("the txpool kind registers without tombstoning the first arm");
        assert_eq!(mev_pump.source(), PendingTxSource::Mevblocker);
        assert_eq!(tx_pump.source(), PendingTxSource::Txpool);

        mev_pump.push(frame(1));
        tx_pump.push(frame(2));

        assert_eq!(
            mev_stream
                .drain()
                .into_iter()
                .map(|t| t.nonce)
                .collect::<Vec<_>>(),
            vec![1],
            "the mevblocker stream is exactly its own feed's corpus"
        );
        assert_eq!(
            tx_stream
                .drain()
                .into_iter()
                .map(|t| t.nonce)
                .collect::<Vec<_>>(),
            vec![2],
            "the txpool stream is exactly its own feed's corpus"
        );
    }

    /// A config that does not match the source kind fails closed: a
    /// miswired host cannot attach a feed to another kind's ring.
    #[test]
    fn a_mismatched_config_fails_closed() {
        let hub = Hub::new();
        assert!(matches!(
            PendingTxPump::spawn_on_ring(
                &hub,
                PendingTxSource::Mevblocker,
                PendingTxFeedConfig::Txpool(TxpoolFeedConfig::defaults()),
            ),
            Err(HubError::PolicyMismatch { .. })
        ));
        assert!(
            hub.registered_classes().is_empty(),
            "the failed mint registered nothing"
        );
    }
}
