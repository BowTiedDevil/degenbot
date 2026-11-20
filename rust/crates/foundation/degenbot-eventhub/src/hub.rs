//! The per-process hub: registration, policy-typed subscription, the head
//! clock, and the overflow channels.
//!
//! A [`Hub`] owns one channel per registered [`HubClass`]. A source calls
//! [`Hub::register_source`] once (declaring its [`OverflowPolicy`]) and keeps
//! the returned [`SourceHandle`]; consumers call [`Hub::subscribe`] to get the
//! matching [`Subscription`]. Both ends share the same channel by `Arc`, so
//! the channel outlives either handle.
//!
//! The head clock is the hub's own, not a registered class: one permanent
//! [`Hub::claim_head_source`] hands the process's one [`HeadSender`] out, and
//! [`Hub::subscribe_head`] is infallible for every reader.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::event::{HubClass, HubEvent};
use crate::head::{Head, HeadSender, HeadShared, HeadSubscription};
use crate::named::{NamedChannel, NamedCounters, NamedReceiver, NamedSender};
use crate::policy::{HubError, OverflowPolicy};

/// Bounded drop-oldest ring, counted on eviction.
struct DropOldestChannel {
    name: &'static str,
    capacity: usize,
    ring: Mutex<VecDeque<HubEvent>>,
    dropped: AtomicU64,
}

impl DropOldestChannel {
    fn new(name: &'static str, capacity: usize) -> Self {
        Self {
            name,
            capacity,
            ring: Mutex::new(VecDeque::with_capacity(capacity)),
            dropped: AtomicU64::new(0),
        }
    }

    fn push(&self, event: HubEvent) {
        let mut ring = self.ring.lock();
        if ring.len() >= self.capacity {
            ring.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        ring.push_back(event);
    }

    fn drain(&self) -> Vec<HubEvent> {
        std::mem::take(&mut *self.ring.lock()).into_iter().collect()
    }
}

/// The typed refusal a second [`Hub::claim_head_source`] gets: the hub's one
/// head source was already claimed, and a claim never lapses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("this hub's head source is already claimed")]
pub struct AlreadyClaimed;

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

/// Unbounded FIFO, lossless by construction and flagged for audit.
struct UnboundedChannel {
    name: &'static str,
    queue: Mutex<VecDeque<HubEvent>>,
}

impl UnboundedChannel {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            queue: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, event: HubEvent) {
        self.queue.lock().push_back(event);
    }

    fn drain(&self) -> Vec<HubEvent> {
        std::mem::take(&mut *self.queue.lock())
            .into_iter()
            .collect()
    }
}

/// The source side of a [`OverflowPolicy::DropOldestCounted`] channel.
#[derive(Clone)]
pub struct DropOldestSender {
    inner: Arc<DropOldestChannel>,
}

impl DropOldestSender {
    /// Append an event, evicting (and counting) the oldest at capacity.
    pub fn push(&self, event: HubEvent) {
        self.inner.push(event);
    }

    /// Evictions counted on this channel.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// The declared counter label.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.inner.name
    }
}

/// The consumer side of a [`OverflowPolicy::DropOldestCounted`] channel.
#[derive(Clone)]
pub struct DropOldestReceiver {
    inner: Arc<DropOldestChannel>,
}

impl DropOldestReceiver {
    /// Take every buffered event, oldest first, atomically.
    #[must_use]
    pub fn drain(&self) -> Vec<HubEvent> {
        self.inner.drain()
    }

    /// Evictions counted on this channel.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// The declared counter label.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.inner.name
    }

    /// The configured ring capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}

/// The source side of a [`OverflowPolicy::UnboundedFlagged`] channel.
#[derive(Clone)]
pub struct UnboundedSender {
    inner: Arc<UnboundedChannel>,
}

impl UnboundedSender {
    /// Append an event; never drops.
    pub fn push(&self, event: HubEvent) {
        self.inner.push(event);
    }
}

/// The consumer side of a [`OverflowPolicy::UnboundedFlagged`] channel.
#[derive(Clone)]
pub struct UnboundedReceiver {
    inner: Arc<UnboundedChannel>,
}

impl UnboundedReceiver {
    /// Take every buffered event, oldest first, atomically.
    #[must_use]
    pub fn drain(&self) -> Vec<HubEvent> {
        self.inner.drain()
    }

    /// The declared audit label.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.inner.name
    }
}

/// The source-side handle handed back by [`Hub::register_source`]; its variant
/// reflects the declared policy.
#[derive(Clone)]
pub enum SourceHandle {
    /// Drop-oldest counted ring.
    DropOldestCounted(DropOldestSender),
    /// Unbounded flagged buffer.
    UnboundedFlagged(UnboundedSender),
}

/// The consumer-side receiver handed back by [`Hub::subscribe`]; its variant
/// reflects the policy declared at registration.
#[derive(Clone)]
pub enum Subscription {
    /// Drop-oldest counted ring.
    DropOldestCounted(DropOldestReceiver),
    /// Unbounded flagged buffer.
    UnboundedFlagged(UnboundedReceiver),
}

enum Channel {
    DropOldestCounted(Arc<DropOldestChannel>),
    UnboundedFlagged(Arc<UnboundedChannel>),
}

struct SourceEntry {
    policy: OverflowPolicy,
    channel: Channel,
}

/// One event hub per host process.
///
/// Sources register exactly once per [`HubClass`]; subscribers receive a
/// policy-typed [`Subscription`]. The hub is transport-pure: it carries its
/// own [`HubEvent`] vocabulary and never names `BotState` or the runtime.
pub struct Hub {
    sources: Mutex<HashMap<HubClass, SourceEntry>>,
    /// The hub's one head clock: the shared receiver state and the unclaimed
    /// sender slot.
    head: HeadSlot,
    /// Named typed source channels, keyed by registration name. Always
    /// [`OverflowPolicy::UnboundedFlagged`]; the hub holds each channel's
    /// consumer end until [`Self::take_named_receiver`] hands it out once.
    named: Mutex<HashMap<&'static str, NamedChannel>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    /// A hub with no registered sources and an unclaimed head clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: Mutex::new(HashMap::new()),
            head: HeadSlot::new(),
            named: Mutex::new(HashMap::new()),
        }
    }

    /// Register the one source for `class`, declaring its overflow policy and
    /// the channel `capacity` (used by `DropOldestCounted`, ignored otherwise).
    ///
    /// # Errors
    ///
    /// [`HubError::AlreadyRegistered`] if `class` already has a source on this
    /// hub.
    pub fn register_source(
        &self,
        class: HubClass,
        policy: OverflowPolicy,
        capacity: usize,
    ) -> Result<SourceHandle, HubError> {
        let mut sources = self.sources.lock();
        if sources.contains_key(&class) {
            return Err(HubError::AlreadyRegistered(class));
        }
        let (channel, handle) = match policy {
            OverflowPolicy::DropOldestCounted { name } => {
                let channel = Arc::new(DropOldestChannel::new(name, capacity));
                (
                    Channel::DropOldestCounted(Arc::clone(&channel)),
                    SourceHandle::DropOldestCounted(DropOldestSender { inner: channel }),
                )
            }
            OverflowPolicy::UnboundedFlagged { name } => {
                let channel = Arc::new(UnboundedChannel::new(name));
                (
                    Channel::UnboundedFlagged(Arc::clone(&channel)),
                    SourceHandle::UnboundedFlagged(UnboundedSender { inner: channel }),
                )
            }
        };
        sources.insert(class, SourceEntry { policy, channel });
        Ok(handle)
    }

    /// Build a drop-oldest ring that this hub does **not** track in its
    /// registry.
    ///
    /// For a consumer that is its own host — one feed whose channel has no
    /// sibling source to collide with (the Python-exposed `BackrunFeed`
    /// constructor). A process host that owns a shared hub uses
    /// [`Self::register_source`] + [`Self::subscribe`] instead.
    #[must_use]
    pub fn detached_drop_oldest(
        &self,
        name: &'static str,
        capacity: usize,
    ) -> (DropOldestSender, DropOldestReceiver) {
        let channel = Arc::new(DropOldestChannel::new(name, capacity));
        (
            DropOldestSender {
                inner: Arc::clone(&channel),
            },
            DropOldestReceiver { inner: channel },
        )
    }

    /// Subscribe to `class`, receiving the policy-typed receiver declared at
    /// registration.
    ///
    /// # Errors
    ///
    /// [`HubError::NotRegistered`] if no source has registered `class`.
    pub fn subscribe(&self, class: HubClass) -> Result<Subscription, HubError> {
        let sources = self.sources.lock();
        let entry = sources.get(&class).ok_or(HubError::NotRegistered(class))?;
        Ok(match &entry.channel {
            Channel::DropOldestCounted(channel) => {
                Subscription::DropOldestCounted(DropOldestReceiver {
                    inner: Arc::clone(channel),
                })
            }
            Channel::UnboundedFlagged(channel) => {
                Subscription::UnboundedFlagged(UnboundedReceiver {
                    inner: Arc::clone(channel),
                })
            }
        })
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

    /// Mint + register a named, typed source channel while constructing the
    /// hub.
    ///
    /// The channel carries a domain payload the vocabulary does not name; its
    /// strictness is always [`OverflowPolicy::UnboundedFlagged`]. Taking
    /// `&mut self` makes registration infallible: a caller holding the only
    /// handle during construction cannot race another registrant. The hub
    /// keeps the consumer end, handed out once by
    /// [`Self::take_named_receiver`].
    #[must_use]
    pub fn add_named_unbounded_source<T: Send + 'static>(
        &mut self,
        name: &'static str,
    ) -> NamedSender<T> {
        let (tx, rx) = mpsc::unbounded_channel::<T>();
        let counters = Arc::new(NamedCounters::new());
        let previous = self
            .named
            .get_mut()
            .insert(name, NamedChannel::new(rx, Arc::clone(&counters)));
        debug_assert!(previous.is_none(), "named hub channel registered twice");
        NamedSender::new(name, tx, counters)
    }

    /// Take the consumer end of a named source channel — once only.
    ///
    /// The hand-off is the depth-tallying [`NamedReceiver`], so every value a
    /// consumer takes is counted and [`Self::named_pending`] sees the channel
    /// drain. A consumer that needs the bare [`mpsc::UnboundedReceiver`] takes
    /// it from [`NamedReceiver::into_inner`], which gives up the tally.
    ///
    /// # Errors
    ///
    /// [`HubError::NamedNotRegistered`] when no channel is registered under
    /// `name`; [`HubError::NamedTypeMismatch`] when `T` is not the payload the
    /// channel was minted with.
    pub fn take_named_receiver<T: Send + 'static>(
        &self,
        name: &'static str,
    ) -> Result<Option<NamedReceiver<T>>, HubError> {
        let mut named = self.named.lock();
        let entry = named
            .get_mut(name)
            .ok_or(HubError::NamedNotRegistered(name))?;
        let Some(boxed) = entry.receiver.get_mut().take() else {
            return Ok(None);
        };
        match boxed.downcast::<NamedReceiver<T>>() {
            Ok(rx) => Ok(Some(*rx)),
            Err(other) => {
                *entry.receiver.get_mut() = Some(other);
                Err(HubError::NamedTypeMismatch(name))
            }
        }
    }

    /// Approximate in-flight depth of the named channel `name` — values sent
    /// but not yet received.
    ///
    /// `None` when no channel is registered under `name`; `Some(0)` for a
    /// registered channel with no in-flight values.
    ///
    /// # Approximation — in-flight tolerance
    ///
    /// The depth is a send/receive tally maintained by [`NamedSender::send`]
    /// and [`NamedReceiver::recv`] / [`NamedReceiver::try_recv`], not a mirror
    /// of tokio's internal queue. Each side records a completed operation, so
    /// a value can be received before the matching send is tallied (or the
    /// reverse); the reported depth may therefore trail or lead the exact
    /// queue length by the number of values in flight, and saturates at zero
    /// rather than underflowing. The raw escape hatches ([`NamedSender::into_inner`] /
    /// [`NamedReceiver::into_inner`]) move values without touching the tally.
    #[must_use]
    pub fn named_pending(&self, name: &'static str) -> Option<u64> {
        self.named.lock().get(name).map(NamedChannel::pending)
    }

    /// The fixed policy of a named source channel, if registered.
    ///
    /// Named channels are always [`OverflowPolicy::UnboundedFlagged`].
    #[must_use]
    pub fn named_policy(&self, name: &'static str) -> Option<OverflowPolicy> {
        self.named
            .lock()
            .contains_key(name)
            .then_some(OverflowPolicy::UnboundedFlagged { name })
    }

    /// The policy declared for `class`, if registered.
    #[must_use]
    pub fn policy_of(&self, class: HubClass) -> Option<OverflowPolicy> {
        self.sources.lock().get(&class).map(|entry| entry.policy)
    }

    /// Every registered class.
    #[must_use]
    pub fn registered_classes(&self) -> Vec<HubClass> {
        self.sources.lock().keys().copied().collect()
    }

    /// Count of registrations using the deliberate unbounded audit posture —
    /// the flagged `HubClass` sources plus every named source channel.
    #[must_use]
    pub fn unbounded_flagged_count(&self) -> usize {
        let named = self.named.lock().len();
        let classes = self
            .sources
            .lock()
            .values()
            .filter(|entry| entry.policy.is_unbounded_flagged())
            .count();
        named + classes
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "hub unit tests assert on registration/subscription outcomes"
)]
mod tests {
    use alloy::primitives::{Address, Bytes, B256, U256};
    use serde_json::Value;

    use super::*;
    use crate::event::{PendingTx, PendingTxSource};

    const NAMED_TEST_CHANNEL: &str = "test_named_channel";

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

    fn nonce_of(event: HubEvent) -> Option<u64> {
        match event {
            HubEvent::PendingTx(tx) => Some(tx.nonce),
            HubEvent::PoolEvent { .. } => None,
        }
    }

    fn drop_oldest_sender(hub: &Hub, capacity: usize) -> DropOldestSender {
        match hub
            .register_source(
                HubClass::PendingTx(PendingTxSource::Mevblocker),
                OverflowPolicy::DropOldestCounted {
                    name: "dropped_ring",
                },
                capacity,
            )
            .expect("registers")
        {
            SourceHandle::DropOldestCounted(sender) => sender,
            SourceHandle::UnboundedFlagged(_) => panic!("declared drop-oldest, got another handle"),
        }
    }

    #[test]
    fn policy_is_fixed_at_registration() {
        let hub = Hub::new();
        assert!(matches!(
            hub.subscribe(HubClass::PendingTx(PendingTxSource::Mevblocker)),
            Err(HubError::NotRegistered(HubClass::PendingTx(
                PendingTxSource::Mevblocker
            )))
        ));
        let handle = drop_oldest_sender(&hub, 4);
        assert_eq!(handle.name(), "dropped_ring");
        assert!(matches!(
            hub.register_source(
                HubClass::PendingTx(PendingTxSource::Mevblocker),
                OverflowPolicy::UnboundedFlagged { name: "late" },
                4,
            ),
            Err(HubError::AlreadyRegistered(HubClass::PendingTx(_)))
        ));
        assert!(matches!(
            hub.subscribe(HubClass::PendingTx(PendingTxSource::Mevblocker)),
            Ok(Subscription::DropOldestCounted(_))
        ));
        assert_eq!(
            hub.policy_of(HubClass::PendingTx(PendingTxSource::Mevblocker)),
            Some(OverflowPolicy::DropOldestCounted {
                name: "dropped_ring"
            })
        );
        assert_eq!(
            hub.registered_classes(),
            vec![HubClass::PendingTx(PendingTxSource::Mevblocker)]
        );
        assert_eq!(hub.unbounded_flagged_count(), 0);
    }

    /// The two pending-tx source kinds register independently on one hub:
    /// neither collides with the other, and each class's ring carries exactly
    /// its own feed's frames (the per-kind corpus split).
    #[test]
    fn both_pending_tx_source_kinds_register_and_drain_independently() {
        let hub = Hub::new();
        let mevblocker = drop_oldest_sender(&hub, 4);
        let SourceHandle::DropOldestCounted(txpool) = hub
            .register_source(
                HubClass::PendingTx(PendingTxSource::Txpool),
                OverflowPolicy::DropOldestCounted {
                    name: "txpool_dropped_ring",
                },
                8,
            )
            .expect("the second kind registers without a tombstone")
        else {
            panic!("declared drop-oldest, got another handle");
        };
        let mut classes = hub.registered_classes();
        classes.sort();
        assert_eq!(
            classes,
            vec![
                HubClass::PendingTx(PendingTxSource::Mevblocker),
                HubClass::PendingTx(PendingTxSource::Txpool),
            ],
            "one registration per class VALUE, not per class variant"
        );

        mevblocker.push(HubEvent::PendingTx(tx(1)));
        txpool.push(HubEvent::PendingTx(tx(2)));

        let Subscription::DropOldestCounted(mevblocker_ring) = hub
            .subscribe(HubClass::PendingTx(PendingTxSource::Mevblocker))
            .expect("subscribed")
        else {
            panic!("policy changed under test");
        };
        let Subscription::DropOldestCounted(txpool_ring) = hub
            .subscribe(HubClass::PendingTx(PendingTxSource::Txpool))
            .expect("subscribed")
        else {
            panic!("policy changed under test");
        };
        let mevblocker_nonces: Vec<u64> = mevblocker_ring
            .drain()
            .into_iter()
            .filter_map(nonce_of)
            .collect();
        let txpool_nonces: Vec<u64> = txpool_ring
            .drain()
            .into_iter()
            .filter_map(nonce_of)
            .collect();
        assert_eq!(
            mevblocker_nonces,
            vec![1],
            "the mevblocker ring is its feed's corpus"
        );
        assert_eq!(
            txpool_nonces,
            vec![2],
            "the txpool ring is its feed's corpus"
        );
    }

    #[test]
    fn drop_oldest_counted_evicts_oldest_and_counts() {
        let hub = Hub::new();
        let sender = drop_oldest_sender(&hub, 4);
        let Subscription::DropOldestCounted(receiver) = hub
            .subscribe(HubClass::PendingTx(PendingTxSource::Mevblocker))
            .expect("subscribed")
        else {
            panic!("policy changed under test");
        };
        for nonce in 0..6 {
            sender.push(HubEvent::PendingTx(tx(nonce)));
        }
        assert_eq!(sender.dropped(), 2);
        assert_eq!(receiver.dropped(), 2);
        assert_eq!(receiver.capacity(), 4);
        let nonces: Vec<u64> = receiver.drain().into_iter().filter_map(nonce_of).collect();
        assert_eq!(nonces, vec![2, 3, 4, 5], "oldest evicted, order preserved");
        assert!(receiver.drain().is_empty(), "drain is an atomic take");
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

    /// The clock is the hub's own, so a `HubClass` registration can never
    /// shadow or duplicate it, and an unclaimed hub answers neither claim twice.
    #[test]
    fn the_head_clock_is_not_a_registered_class() {
        let hub = Hub::new();
        assert!(hub.registered_classes().is_empty());
        let _sender = hub
            .claim_head_source()
            .expect("the fresh clock is unclaimed");
        assert!(
            hub.registered_classes().is_empty(),
            "claiming the head source registers no hub class"
        );
    }

    #[test]
    fn unbounded_flagged_stays_lossless() {
        let hub = Hub::new();
        let handle = hub
            .register_source(
                HubClass::PoolEvent,
                OverflowPolicy::UnboundedFlagged {
                    name: "engine_results",
                },
                0,
            )
            .expect("registers");
        assert_eq!(hub.unbounded_flagged_count(), 1);
        let SourceHandle::UnboundedFlagged(sender) = handle else {
            panic!("declared unbounded, got another handle");
        };
        let Subscription::UnboundedFlagged(receiver) =
            hub.subscribe(HubClass::PoolEvent).expect("subscribed")
        else {
            panic!("policy changed under test");
        };
        for nonce in 0..100 {
            sender.push(HubEvent::PendingTx(tx(nonce)));
        }
        let nonces: Vec<u64> = receiver.drain().into_iter().filter_map(nonce_of).collect();
        assert_eq!(nonces, (0..100).collect::<Vec<_>>());
        assert_eq!(receiver.name(), "engine_results");
        assert!(receiver.drain().is_empty());
    }

    #[test]
    fn named_source_is_unbounded_flagged_and_drops_nothing_across_bursts() {
        const BURSTS: u64 = 4;
        const PER_BURST: u64 = 300;
        let mut hub = Hub::new();
        let tx = hub
            .add_named_unbounded_source::<u64>(NAMED_TEST_CHANNEL)
            .into_inner();
        assert_eq!(
            hub.named_policy(NAMED_TEST_CHANNEL),
            Some(OverflowPolicy::UnboundedFlagged {
                name: NAMED_TEST_CHANNEL
            })
        );
        assert!(
            hub.named_policy(NAMED_TEST_CHANNEL)
                .expect("registered")
                .is_unbounded_flagged(),
            "a named source is the deliberate unbounded audit posture"
        );
        assert_eq!(hub.unbounded_flagged_count(), 1);
        let mut rx = hub
            .take_named_receiver::<u64>(NAMED_TEST_CHANNEL)
            .expect("registered")
            .expect("fresh channel");
        for burst in 0..BURSTS {
            for i in 0..PER_BURST {
                let _ = tx.send(burst * PER_BURST + i);
            }
        }
        let mut seen = Vec::new();
        while let Ok(value) = rx.try_recv() {
            seen.push(value);
        }
        assert_eq!(seen, (0..BURSTS * PER_BURST).collect::<Vec<u64>>());
        assert_eq!(hub.unbounded_flagged_count(), 1);
    }

    #[tokio::test]
    async fn named_channel_round_trip_matches_raw_mpsc() {
        const BURSTS: u64 = 8;
        const PER_BURST: u64 = 500;
        // Pre shape: the bare unbounded mpsc the engine used before the hub.
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<u64>();
        // Post shape: the same primitive registered as a named hub source.
        let mut hub = Hub::new();
        let hub_tx = hub.add_named_unbounded_source::<u64>(NAMED_TEST_CHANNEL);
        let mut hub_rx = hub
            .take_named_receiver::<u64>(NAMED_TEST_CHANNEL)
            .expect("registered")
            .expect("fresh channel");
        for burst in 0..BURSTS {
            for i in 0..PER_BURST {
                let value = burst * PER_BURST + i;
                assert!(raw_tx.send(value).is_ok());
                assert!(hub_tx.send(value).is_ok());
            }
        }
        assert_eq!(
            hub.named_pending(NAMED_TEST_CHANNEL),
            Some(BURSTS * PER_BURST),
            "the hub hand-off tallies every in-flight value"
        );
        let mut raw_seen = Vec::new();
        while let Ok(value) = raw_rx.try_recv() {
            raw_seen.push(value);
        }
        let mut hub_seen = Vec::new();
        while let Ok(value) = hub_rx.try_recv() {
            hub_seen.push(value);
        }
        assert_eq!(
            raw_seen.len(),
            usize::try_from(BURSTS * PER_BURST).expect("fits in usize")
        );
        assert_eq!(hub_seen, raw_seen);
        assert_eq!(
            hub.named_pending(NAMED_TEST_CHANNEL),
            Some(0),
            "draining the hub hand-off returns the depth tally to zero"
        );
    }

    #[test]
    fn named_receiver_is_handed_out_once_and_typed() {
        let mut hub = Hub::new();
        let _tx = hub.add_named_unbounded_source::<u64>(NAMED_TEST_CHANNEL);
        assert!(matches!(
            hub.take_named_receiver::<String>(NAMED_TEST_CHANNEL),
            Err(HubError::NamedTypeMismatch(NAMED_TEST_CHANNEL))
        ));
        assert!(hub
            .take_named_receiver::<u64>(NAMED_TEST_CHANNEL)
            .expect("registered")
            .is_some());
        assert!(hub
            .take_named_receiver::<u64>(NAMED_TEST_CHANNEL)
            .expect("registered")
            .is_none());
        assert!(matches!(
            hub.take_named_receiver::<u64>("missing_channel"),
            Err(HubError::NamedNotRegistered("missing_channel"))
        ));
    }

    #[test]
    fn named_pending_counts_sends_before_any_receive() {
        const NAME: &str = "pending_no_recv";
        let mut hub = Hub::new();
        let tx = hub.add_named_unbounded_source::<u64>(NAME);
        assert_eq!(hub.named_pending(NAME), Some(0), "fresh channel is empty");
        for i in 0..7 {
            tx.send(i).expect("sends");
        }
        assert_eq!(
            hub.named_pending(NAME),
            Some(7),
            "no receiver has drained the queue, so growth is visible"
        );
        assert_eq!(tx.pending(), 7);
    }

    #[test]
    fn named_pending_returns_to_zero_after_an_ordered_drain() {
        const NAME: &str = "pending_drain";
        let mut hub = Hub::new();
        let tx = hub.add_named_unbounded_source::<u64>(NAME);
        for i in 0..5 {
            tx.send(i).expect("sends");
        }
        let mut rx = hub
            .take_named_receiver::<u64>(NAME)
            .expect("registered")
            .expect("fresh channel");
        assert_eq!(hub.named_pending(NAME), Some(5));
        let mut seen = Vec::new();
        while let Ok(value) = rx.try_recv() {
            seen.push(value);
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4], "FIFO drain preserved");
        assert_eq!(hub.named_pending(NAME), Some(0));
        assert_eq!(rx.pending(), 0);

        for i in 5..9 {
            tx.send(i).expect("sends");
        }
        assert_eq!(hub.named_pending(NAME), Some(4));
        assert_eq!(rx.try_recv(), Ok(5), "value shape unchanged");
        assert_eq!(hub.named_pending(NAME), Some(3));
    }

    #[test]
    fn named_pending_is_zero_with_no_traffic_and_none_when_unregistered() {
        const NAME: &str = "pending_zeros";
        let mut hub = Hub::new();
        let _tx = hub.add_named_unbounded_source::<u64>(NAME);
        assert_eq!(hub.named_pending(NAME), Some(0));
        assert_eq!(hub.named_pending("missing_pending_channel"), None);
    }

    #[tokio::test]
    async fn named_receiver_recv_tallies_the_take() {
        const NAME: &str = "pending_async_recv";
        let mut hub = Hub::new();
        let tx = hub.add_named_unbounded_source::<u64>(NAME);
        let mut rx = hub
            .take_named_receiver::<u64>(NAME)
            .expect("registered")
            .expect("fresh channel");
        tx.send(11).expect("sends");
        assert_eq!(hub.named_pending(NAME), Some(1));
        assert_eq!(rx.recv().await, Some(11));
        assert_eq!(hub.named_pending(NAME), Some(0));
    }
}
