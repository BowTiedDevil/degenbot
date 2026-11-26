//! The typed pending-tx stream a hosted driver drains, and the pump its host
//! keeps.
//!
//! One ring per source kind ([`PendingTxSource`]): the `MEVBlocker` searcher
//! feed and the chain-node txpool feed are opened, published, and drained
//! independently, so each arm's frame corpus is exactly its own feed's and a
//! second arm can run in the same process.
//!
//! One provisioning act, two ends: [`PendingTxPump`] is the host-side handle
//! (feed telemetry, shutdown) and owns the ring's publisher through its feed;
//! [`PendingTxStream`] is the driver-side typed drain and owns the ring's one
//! view. Neither side holds an optional view, and the stream is deliberately
//! not `Clone`, so a ring has exactly one drainer.
//!
//! Provisioning and starting are split: [`PendingTxPump::mint`] opens the ring
//! and hands out the stream but spawns nothing, and [`PendingTxPump::start`]
//! launches the feed loop — the first dial — later. A host that mints pumps and
//! never starts them performs no network I/O.

use std::num::NonZeroUsize;

#[cfg(any(feature = "test-utils", test))]
use degenbot_eventhub::PendingTxPublisher;
use degenbot_eventhub::{Hub, HubError, PendingTx, PendingTxSource, PendingTxView};

use crate::backrun_feed::{BackrunFeed, BackrunFeedConfig, BackrunFeedStatus};
use crate::txpool_feed::{TxpoolFeed, TxpoolFeedConfig};

/// The per-kind feed configuration a host mints a pump from.
///
/// The variant IS the source kind, so a pump cannot be minted for one kind over
/// another kind's feed: the mismatch is unrepresentable rather than refused.
#[derive(Debug, Clone)]
pub enum PendingTxFeedConfig {
    /// The `MEVBlocker` searcher feed.
    Mevblocker(BackrunFeedConfig),
    /// The chain-node txpool feed.
    Txpool(TxpoolFeedConfig),
}

impl PendingTxFeedConfig {
    /// The source kind this config names.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        match self {
            Self::Mevblocker(_) => PendingTxSource::Mevblocker,
            Self::Txpool(_) => PendingTxSource::Txpool,
        }
    }

    /// The ring capacity this config names.
    #[must_use]
    pub fn ring_capacity(&self) -> NonZeroUsize {
        match self {
            Self::Mevblocker(cfg) => cfg.ring_capacity,
            Self::Txpool(cfg) => cfg.ring_capacity,
        }
    }
}

/// The host-side pump handle over one source kind's feed.
///
/// The hosted-sources owner keeps this for the feed-telemetry sampler and
/// shutdown; the frames flow to the [`PendingTxStream`] handed out at mint.
pub struct PendingTxPump {
    source: PendingTxSource,
    /// The ring's publishing end, kept for the test-support frame injection
    /// only (the live feed owns the real publishing path).
    #[cfg(any(feature = "test-utils", test))]
    publisher: PendingTxPublisher,
    inner: PumpInner,
}

enum PumpInner {
    Mevblocker(BackrunFeed),
    Txpool(TxpoolFeed),
}

/// The driver-side typed drain over one source kind's hub ring.
///
/// The ring only ever carries this kind's [`PendingTx`] frames, and it has
/// exactly one view — this one.
pub struct PendingTxStream {
    source: PendingTxSource,
    view: PendingTxView,
}

impl PendingTxPump {
    /// Open `cfg`'s ring on `hub` and return the pump that publishes to it plus
    /// the arm's typed [`PendingTxStream`]. Spawns NOTHING and dials nothing:
    /// the feed loop launches later through [`Self::start`], so a host can mint
    /// a pump without touching the network.
    ///
    /// # Errors
    ///
    /// [`HubError::AlreadyRegistered`] when `hub` already carries a ring for
    /// this source kind — one ring per kind, so a second arm cannot open a
    /// second drainer.
    pub fn mint(hub: &Hub, cfg: PendingTxFeedConfig) -> Result<(Self, PendingTxStream), HubError> {
        let source = cfg.source();
        let (publisher, view) = hub.open_pending_tx_ring(source, cfg.ring_capacity())?;
        let inner = match cfg {
            PendingTxFeedConfig::Mevblocker(cfg) => {
                PumpInner::Mevblocker(BackrunFeed::build(cfg, publisher.clone()))
            }
            PendingTxFeedConfig::Txpool(cfg) => {
                PumpInner::Txpool(TxpoolFeed::build(cfg, publisher.clone()))
            }
        };
        #[cfg(any(feature = "test-utils", test))]
        let pump = Self {
            source,
            publisher,
            inner,
        };
        #[cfg(not(any(feature = "test-utils", test)))]
        let pump = Self { source, inner };
        Ok((pump, PendingTxStream { source, view }))
    }

    /// The source kind this pump provisions.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        self.source
    }

    /// Launch this pump's feed loop — its first dial — exactly once. The ring
    /// and the driver's stream were provisioned at [`Self::mint`] and are
    /// already readable; only the reconnect loop starts here.
    pub fn start(&self) {
        match &self.inner {
            PumpInner::Mevblocker(feed) => feed.start(),
            PumpInner::Txpool(feed) => feed.start(),
        }
    }

    /// The sampler snapshot (the `BackrunFeedStatus` field set both pumps
    /// share; the txpool feed's extra `mine_misses` counter is pump-scoped
    /// forensics, not an engine instrument).
    #[must_use]
    pub fn status(&self) -> BackrunFeedStatus {
        match &self.inner {
            PumpInner::Mevblocker(feed) => feed.status(),
            PumpInner::Txpool(feed) => feed.status().into(),
        }
    }

    /// The txpool arm's mine-miss counter (a by-hash result that came back
    /// `null` because the tx mined first), or `None` for a pump whose feed has
    /// no such counter.
    #[must_use]
    pub fn mine_misses(&self) -> Option<u64> {
        match &self.inner {
            PumpInner::Mevblocker(_) => None,
            PumpInner::Txpool(feed) => Some(feed.status().mine_misses),
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
    pub fn push(&self, frame: PendingTx) {
        self.publisher.push(frame);
    }
}

impl PendingTxStream {
    /// The source kind this stream drains.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        self.source
    }

    /// All buffered frames, ordered, atomically (rings swap; concurrent drains
    /// may interleave but never duplicate or lose between both buffers).
    #[must_use]
    pub fn drain(&self) -> Vec<PendingTx> {
        self.view.drain()
    }

    /// Evictions counted on this stream's ring.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.view.dropped()
    }

    /// The ring capacity this stream drains.
    #[must_use]
    pub fn capacity(&self) -> NonZeroUsize {
        self.view.capacity()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "unit tests unwrap the ring openings they assert on"
)]
mod tests {
    use std::num::NonZeroUsize;

    use degenbot_eventhub::detached_pending_ring;

    use super::*;
    use crate::backrun_feed::BackrunFeedConfig;
    use crate::txpool_feed::TxpoolFeedConfig;

    fn capacity(capacity: usize) -> NonZeroUsize {
        NonZeroUsize::new(capacity).expect("a test capacity is non-zero")
    }

    fn frame(nonce: u64) -> PendingTx {
        PendingTx {
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
        }
    }

    fn nonces(frames: Vec<PendingTx>) -> Vec<u64> {
        frames.into_iter().map(|frame| frame.nonce).collect()
    }

    /// Both source kinds mint on ONE hub: neither registration tombstones the
    /// other, and each stream's corpus is exactly its own feed's.
    #[test]
    fn both_kinds_mint_on_one_hub_and_drain_their_own_corpus() {
        let hub = Hub::new();
        let (mev_pump, mev_stream) = PendingTxPump::mint(
            &hub,
            PendingTxFeedConfig::Mevblocker(BackrunFeedConfig::for_mainnet()),
        )
        .expect("the mevblocker kind opens its ring");
        let (tx_pump, tx_stream) = PendingTxPump::mint(
            &hub,
            PendingTxFeedConfig::Txpool(TxpoolFeedConfig::defaults()),
        )
        .expect("the txpool kind opens without tombstoning the first arm");

        assert_eq!(mev_pump.source(), PendingTxSource::Mevblocker);
        assert_eq!(tx_pump.source(), PendingTxSource::Txpool);

        mev_pump.push(frame(1));
        tx_pump.push(frame(2));

        assert_eq!(
            nonces(mev_stream.drain()),
            vec![1],
            "the mevblocker stream is exactly its own feed's corpus"
        );
        assert_eq!(
            nonces(tx_stream.drain()),
            vec![2],
            "the txpool stream is exactly its own feed's corpus"
        );
    }

    /// One drainer per ring, on both paths that open one: the hub refuses a
    /// second ring for a kind (so a hosted arm cannot grow a second view), and
    /// the eager path's detached ring hands out one view — `PendingTxView` is
    /// deliberately not `Clone`, which is the compile-time half of the same
    /// guarantee.
    #[test]
    fn eager_and_hosted_rings_have_one_drainer() {
        let hub = Hub::new();
        let cfg = PendingTxFeedConfig::Mevblocker(BackrunFeedConfig::for_mainnet());
        let (_pump, hosted) =
            PendingTxPump::mint(&hub, cfg.clone()).expect("the hosted ring opens");
        assert_eq!(
            PendingTxPump::mint(&hub, cfg).err(),
            Some(HubError::AlreadyRegistered(PendingTxSource::Mevblocker)),
            "the hosted kind already has its one ring and its one drainer"
        );

        let (_publisher, eager) = detached_pending_ring(capacity(8));
        assert_eq!(eager.capacity(), capacity(8));
        assert_eq!(hosted.capacity(), cfg_ring_capacity());
    }

    fn cfg_ring_capacity() -> NonZeroUsize {
        BackrunFeedConfig::for_mainnet().ring_capacity
    }
}
