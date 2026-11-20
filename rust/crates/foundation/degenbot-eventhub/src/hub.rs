//! The per-process hub: typed pending rings and the head clock.
//!
//! A [`Hub`] owns one pending-transaction ring per [`PendingTxSource`]. A feed
//! calls [`Hub::open_pending_tx_ring`] once, keeping the returned
//! [`PendingTxPublisher`]; the ring's one [`PendingTxView`] goes to the
//! consumer that drains it. Both ends share the ring by `Arc`, so the ring
//! outlives either handle.
//!
//! The head clock is the hub's own, not a ring: one permanent
//! [`Hub::claim_head_source`] hands the process's one [`HeadSender`] out, and
//! [`Hub::subscribe_head`] is infallible for every reader.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;

use crate::event::PendingTxSource;
use crate::head::{Head, HeadSender, HeadShared, HeadSubscription};
use crate::pending::{PendingTxPublisher, PendingTxRing, PendingTxView};

/// The typed refusal a second [`Hub::claim_head_source`] gets: the hub's one
/// head source was already claimed, and a claim never lapses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("this hub's head source is already claimed")]
pub struct AlreadyClaimed;

/// Hub registration failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HubError {
    /// A ring for this source kind is already open on this hub.
    #[error("pending-tx ring for {0:?} is already open on this hub")]
    AlreadyRegistered(PendingTxSource),
}

/// The hub's one head clock: the unclaimed sender slot and the shared
/// receiver state.
///
/// `sender` is the permanent `Unclaimed -> Claimed` transition: [`Hub::claim_head_source`]
/// takes it out once and never puts it back, so a claim never lapses and a
/// second claim is always refused. The never-claimed slot sender is what closes
/// a never-claimed clock's subscriptions at hub drop.
struct HeadSlot {
    sender: Mutex<Option<watch::Sender<Option<Head>>>>,
    shared: Arc<HeadShared>,
}

impl HeadSlot {
    fn new() -> Self {
        let (tx, _rx) = watch::channel(None);
        let shared = Arc::new(HeadShared::new(&tx));
        Self {
            sender: Mutex::new(Some(tx)),
            shared,
        }
    }
}

/// One event hub per host process.
///
/// A source opens its kind's ring exactly once; the ring's publisher and its
/// one view carry the typed [`crate::PendingTx`] frames. The hub is
/// transport-pure: it carries no runtime state and names no `BotState`.
pub struct Hub {
    pending: Mutex<HashMap<PendingTxSource, Arc<PendingTxRing>>>,
    /// The hub's one head clock: the shared receiver state and the unclaimed
    /// sender slot.
    head: HeadSlot,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    /// A hub with no pending ring and an unclaimed head clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            head: HeadSlot::new(),
        }
    }

    /// Open `source`'s pending ring at `capacity` and hand back its two ends.
    ///
    /// The capacity is a [`NonZeroUsize`], so a zero-capacity ring cannot be
    /// requested. The publisher is the feed's end; the view is the one
    /// consumer's end and cannot be cloned.
    ///
    /// # Errors
    ///
    /// [`HubError::AlreadyRegistered`] if `source`'s ring is already open on
    /// this hub: one ring per source kind, so a second consumer would be a
    /// second drainer.
    pub fn open_pending_tx_ring(
        &self,
        source: PendingTxSource,
        capacity: NonZeroUsize,
    ) -> Result<(PendingTxPublisher, PendingTxView), HubError> {
        let mut pending = self.pending.lock();
        if pending.contains_key(&source) {
            return Err(HubError::AlreadyRegistered(source));
        }
        let ring = Arc::new(PendingTxRing::new(capacity));
        pending.insert(source, Arc::clone(&ring));
        Ok((
            PendingTxPublisher::new(Arc::clone(&ring)),
            PendingTxView::new(ring),
        ))
    }

    /// Claim this hub's one head source: the permanent `Unclaimed -> Claimed`
    /// transition.
    ///
    /// The claimer is the process's one head publisher — the engine block
    /// pump's accepted-header edge, a hosted `newHeads` watch, or a standalone
    /// driver's pump. Every publisher handle is a clone of the returned
    /// [`HeadSender`], so a claimed clock's subscriptions close exactly when
    /// the last clone drops.
    ///
    /// # Errors
    ///
    /// [`AlreadyClaimed`] — the transition is permanent. A claim never lapses,
    /// and a second claim is refused even after every publisher has dropped.
    pub fn claim_head_source(&self) -> Result<HeadSender, AlreadyClaimed> {
        let Some(tx) = self.head.sender.lock().take() else {
            return Err(AlreadyClaimed);
        };
        self.head.shared.reset_clock();
        Ok(HeadSender::new(tx, Arc::clone(&self.head.shared)))
    }

    /// Whether this hub's head source has been claimed — the `PumpOwned` start
    /// check. False only while the slot still holds its sender; it stays true
    /// forever after a claim, whatever the publishers do.
    #[must_use]
    pub fn head_source_is_claimed(&self) -> bool {
        self.head.sender.lock().is_none()
    }

    /// Subscribe to the hub head source.
    ///
    /// Infallible and multi-subscriber: the subscription is live at the
    /// clock's CURRENT value, so a subscription created after a head was
    /// published sees that head without treating it as a new edge. The
    /// subscription closes when the claimer's publishers drop (a claimed
    /// clock) or when the hub drops (a never-claimed clock).
    #[must_use]
    pub fn subscribe_head(&self) -> HeadSubscription {
        self.head.shared.subscribe()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "hub unit tests unwrap the ring openings and the one head claim"
)]
mod tests {
    use std::num::NonZeroUsize;

    use alloy::primitives::{Address, Bytes, B256, U256};
    use serde_json::Value;

    use super::*;
    use crate::event::{PendingTx, PendingTxSource};

    fn capacity(capacity: usize) -> NonZeroUsize {
        NonZeroUsize::new(capacity).expect("a test capacity is non-zero")
    }

    fn tx(nonce: u64) -> PendingTx {
        PendingTx {
            raw_signed_tx: None,
            chain_id: 1,
            from: Address::ZERO,
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            gas: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
            hash: B256::ZERO,
            access_list: Value::Null,
            tx_type: 2,
            received_unix_ms: 0,
        }
    }

    fn nonces(frames: Vec<PendingTx>) -> Vec<u64> {
        frames.into_iter().map(|frame| frame.nonce).collect()
    }

    /// The two pending-tx source kinds open independently on one hub: neither
    /// collides with the other, and each ring carries exactly its own feed's
    /// frames (the per-kind corpus split).
    #[test]
    fn both_pending_tx_source_kinds_register_and_drain_independently() {
        let hub = Hub::new();
        let (mevblocker, mevblocker_ring) = hub
            .open_pending_tx_ring(PendingTxSource::Mevblocker, capacity(4))
            .expect("the mevblocker kind opens");
        let (txpool, txpool_ring) = hub
            .open_pending_tx_ring(PendingTxSource::Txpool, capacity(8))
            .expect("the txpool kind opens without a tombstone");

        mevblocker.push(tx(1));
        txpool.push(tx(2));

        assert_eq!(
            nonces(mevblocker_ring.drain()),
            vec![1],
            "the mevblocker ring is its feed's corpus"
        );
        assert_eq!(
            nonces(txpool_ring.drain()),
            vec![2],
            "the txpool ring is its feed's corpus"
        );
    }

    /// One ring per source kind: the second open is refused, so a kind cannot
    /// grow a second drainer.
    #[test]
    fn a_second_registration_of_one_kind_is_refused() {
        let hub = Hub::new();
        let _first = hub
            .open_pending_tx_ring(PendingTxSource::Mevblocker, capacity(4))
            .expect("the first ring opens");
        assert_eq!(
            hub.open_pending_tx_ring(PendingTxSource::Mevblocker, capacity(4))
                .err(),
            Some(HubError::AlreadyRegistered(PendingTxSource::Mevblocker)),
            "the kind's ring is already open"
        );
        assert!(
            hub.open_pending_tx_ring(PendingTxSource::Txpool, capacity(4))
                .is_ok(),
            "the refused second open does not tombstone the other kind"
        );
    }

    #[test]
    fn drop_oldest_counted_evicts_oldest_and_counts() {
        let hub = Hub::new();
        let (publisher, ring) = hub
            .open_pending_tx_ring(PendingTxSource::Mevblocker, capacity(4))
            .expect("the ring opens");

        for nonce in 0..6 {
            publisher.push(tx(nonce));
        }

        assert_eq!(publisher.dropped(), 2);
        assert_eq!(ring.dropped(), 2);
        assert_eq!(ring.capacity(), capacity(4));
        assert_eq!(
            nonces(ring.drain()),
            vec![2, 3, 4, 5],
            "oldest evicted, order preserved"
        );
        assert!(ring.drain().is_empty(), "drain is an atomic take");
    }

    /// A capacity-one ring holds only the newest frame: every older frame is
    /// evicted and counted, never retained beside the new one.
    #[test]
    fn capacity_one_evicts_every_older_frame() {
        let hub = Hub::new();
        let (publisher, ring) = hub
            .open_pending_tx_ring(PendingTxSource::Txpool, capacity(1))
            .expect("the ring opens");

        for nonce in 0..3 {
            publisher.push(tx(nonce));
        }

        assert_eq!(publisher.dropped(), 2, "each push evicts its predecessor");
        assert_eq!(ring.capacity(), capacity(1));
        assert_eq!(nonces(ring.drain()), vec![2], "only the newest frame stays");
    }

    /// Drop telemetry belongs to the publisher: a feed reports its own
    /// evictions with no consumer view in existence.
    #[test]
    fn publisher_drop_telemetry_needs_no_consumer_view() {
        let hub = Hub::new();
        let (publisher, ring) = hub
            .open_pending_tx_ring(PendingTxSource::Mevblocker, capacity(2))
            .expect("the ring opens");
        drop(ring);

        for nonce in 0..5 {
            publisher.push(tx(nonce));
        }

        assert_eq!(
            publisher.dropped(),
            3,
            "the publisher counts the evictions its ring performed"
        );
    }

    fn head(number: u64) -> Head {
        Head {
            number,
            metadata: None,
        }
    }

    /// Teardown closure: a claimed clock's subscriptions end when every
    /// publisher drops. The receiver must not keep the sender alive.
    #[tokio::test]
    async fn dropping_every_head_sender_closes_subscriptions() {
        use std::time::Duration;

        let hub = Hub::new();
        let sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        let mut first = hub.subscribe_head();
        let mut second = hub.subscribe_head();
        let publisher = sender.clone();
        sender.publish(head(1));
        assert!(first.changed().await.is_ok());
        assert_eq!(first.borrow_and_update(), Some(head(1)));
        assert!(second.changed().await.is_ok());
        assert_eq!(second.borrow_and_update(), Some(head(1)));

        drop(sender);
        drop(publisher);

        let closed = tokio::time::timeout(Duration::from_secs(1), first.changed()).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "a claimed clock closes when every publisher drops: {closed:?}"
        );
        let closed = tokio::time::timeout(Duration::from_secs(1), second.changed()).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "every subscription closes with the last publisher: {closed:?}"
        );
    }

    /// A clock nobody claimed has no publisher to lose: its subscriptions stay
    /// open until the hub drops the slot sender.
    #[tokio::test]
    async fn a_never_claimed_clock_closes_only_on_hub_drop() {
        use std::time::Duration;

        let hub = Hub::new();
        let mut head = hub.subscribe_head();
        assert!(
            !hub.head_source_is_claimed(),
            "a fresh hub's head source is unclaimed"
        );
        let quiet = tokio::time::timeout(Duration::from_millis(50), head.changed()).await;
        assert!(
            quiet.is_err(),
            "a never-claimed clock is open, not closed: {quiet:?}"
        );

        drop(hub);

        let closed = tokio::time::timeout(Duration::from_secs(1), head.changed()).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "the hub's drop closes the never-claimed clock: {closed:?}"
        );
    }

    /// A subscription is live at the clock's CURRENT value: the head published
    /// before it attached is seen immediately, never replayed as a new edge.
    #[tokio::test]
    async fn a_late_subscription_does_not_replay_the_current_head_as_new() {
        use std::time::Duration;

        let hub = Hub::new();
        let sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        sender.publish(head(7));
        let mut late = hub.subscribe_head();
        assert_eq!(
            late.head(),
            Some(head(7)),
            "a late subscriber sees the current head"
        );
        let edge = tokio::time::timeout(Duration::from_millis(50), late.changed()).await;
        assert!(
            edge.is_err(),
            "the current head is not a new edge for a late subscriber: {edge:?}"
        );

        sender.publish(head(8));
        assert!(late.changed().await.is_ok());
        assert_eq!(
            late.borrow_and_update(),
            Some(head(8)),
            "the next published head is the edge"
        );
    }

    /// The staleness clock is seeded at hub construction: an unclaimed clock is
    /// fresh, so a start-time check cannot read an unclaimed clock as stale.
    #[test]
    fn fresh_before_claim_is_not_stale() {
        use std::time::Duration;

        let hub = Hub::new();
        let head = hub.subscribe_head();
        assert!(
            !head.stale(Duration::from_millis(1)),
            "a hub just constructed has a fresh clock"
        );
    }

    /// The claim resets the clock: a hub that sat unclaimed for a long time
    /// hands its claimer a fresh clock, not one already past a stale bound.
    #[test]
    fn fresh_at_claim_is_not_stale() {
        use std::time::Duration;

        let hub = Hub::new();
        let head = hub.subscribe_head();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            head.stale(Duration::from_millis(10)),
            "an unclaimed clock ages with the hub"
        );
        let _sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        assert!(
            !head.stale(Duration::from_millis(10)),
            "the claim resets the clock every consumer reads"
        );
    }

    /// A clock that published and then went quiet reports stale past the
    /// threshold: the predicate the loop's fallback poll rides.
    #[tokio::test]
    async fn quiet_past_threshold_is_stale() {
        use std::time::Duration;

        let hub = Hub::new();
        let sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        sender.publish(head(12));
        let probe = hub.subscribe_head();
        assert!(
            !probe.stale(Duration::from_secs(30)),
            "a just-published clock is fresh"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            probe.stale(Duration::from_millis(10)),
            "quiet past the threshold is stale"
        );
    }

    /// The claim is permanent: dropping every publisher closes the clock's
    /// subscriptions but never re-opens the claim slot.
    #[tokio::test]
    async fn a_claim_never_lapses_when_publishers_drop() {
        let hub = Hub::new();
        let sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        assert!(hub.head_source_is_claimed());
        drop(sender);
        assert!(
            hub.head_source_is_claimed(),
            "the claim survives every publisher"
        );
        assert!(
            matches!(hub.claim_head_source(), Err(AlreadyClaimed)),
            "a second claim is always refused"
        );
    }
}
