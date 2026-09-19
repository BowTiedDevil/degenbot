//! The per-process hub: registration, policy-typed subscription, and the
//! three overflow channels.
//!
//! A [`Hub`] owns one channel per registered [`HubClass`]. A source calls
//! [`Hub::register_source`] once (declaring its [`OverflowPolicy`]) and keeps
//! the returned [`SourceHandle`]; consumers call [`Hub::subscribe`] to get the
//! matching [`Subscription`]. Both ends share the same channel by `Arc`, so
//! the channel outlives either handle.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::event::{HubClass, HubEvent};
use crate::head::{HeadSender, HeadSubscription};
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

/// Latest-value slot; each push supersedes the previous value and wakes
/// every awaiting consumer.
///
/// A `watch` channel supplies both the latest-value semantics and the async
/// wakeup; `last_seen` is the hub-owned staleness clock (set at registration
/// and refreshed on every push).
struct LatestChannel {
    tx: tokio::sync::watch::Sender<Option<HubEvent>>,
    last_seen: Mutex<Option<Instant>>,
}

impl LatestChannel {
    fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(None);
        Self {
            tx,
            // A just-registered source is not immediately stale (matching
            // the transport's "fresh at subscribe" contract).
            last_seen: Mutex::new(Some(Instant::now())),
        }
    }

    fn push(&self, event: HubEvent) {
        self.tx.send_replace(Some(event));
        *self.last_seen.lock() = Some(Instant::now());
    }

    fn latest(&self) -> Option<HubEvent> {
        self.tx.borrow().clone()
    }

    fn last_seen_age(&self) -> Option<Duration> {
        self.last_seen.lock().map(|at| at.elapsed())
    }

    fn stale(&self, threshold: Duration) -> bool {
        match self.last_seen_age() {
            Some(age) => age > threshold,
            None => true,
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

/// The source side of a [`OverflowPolicy::LatestOnly`] channel.
#[derive(Clone)]
pub struct LatestSender {
    inner: Arc<LatestChannel>,
}

impl LatestSender {
    /// Publish a value, superseding any previous one.
    pub fn push(&self, event: HubEvent) {
        self.inner.push(event);
    }

    /// Time since the last publish, or `None` if none was ever made.
    #[must_use]
    pub fn last_seen_age(&self) -> Option<Duration> {
        self.inner.last_seen_age()
    }

    /// Whether no value has arrived for at least `threshold`.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        self.inner.stale(threshold)
    }
}

/// The consumer side of a [`OverflowPolicy::LatestOnly`] channel.
#[derive(Clone)]
pub struct LatestReceiver {
    inner: Arc<LatestChannel>,
    rx: tokio::sync::watch::Receiver<Option<HubEvent>>,
}

impl LatestReceiver {
    /// The most recent value, if any.
    #[must_use]
    pub fn latest(&self) -> Option<HubEvent> {
        self.inner.latest()
    }

    /// The most recent value, marking it seen for the next [`Self::changed`].
    #[must_use]
    pub fn latest_and_update(&mut self) -> Option<HubEvent> {
        self.rx.borrow_and_update().clone()
    }

    /// Wait until a newer value is published.
    ///
    /// # Errors
    ///
    /// Fails once every sender clone has dropped and no newer value will
    /// arrive.
    pub async fn changed(&mut self) -> Result<(), tokio::sync::watch::error::RecvError> {
        self.rx.changed().await
    }

    /// Time since the last publish, or `None` if none was ever made.
    #[must_use]
    pub fn last_seen_age(&self) -> Option<Duration> {
        self.inner.last_seen_age()
    }

    /// Whether no value has arrived for at least `threshold`.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        self.inner.stale(threshold)
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
    /// Latest-value slot.
    LatestOnly(LatestSender),
    /// Unbounded flagged buffer.
    UnboundedFlagged(UnboundedSender),
}

/// The consumer-side receiver handed back by [`Hub::subscribe`]; its variant
/// reflects the policy declared at registration.
#[derive(Clone)]
pub enum Subscription {
    /// Drop-oldest counted ring.
    DropOldestCounted(DropOldestReceiver),
    /// Latest-value slot.
    LatestOnly(LatestReceiver),
    /// Unbounded flagged buffer.
    UnboundedFlagged(UnboundedReceiver),
}

enum Channel {
    DropOldestCounted(Arc<DropOldestChannel>),
    LatestOnly(Arc<LatestChannel>),
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
#[derive(Default)]
pub struct Hub {
    sources: Mutex<HashMap<HubClass, SourceEntry>>,
}

impl Hub {
    /// A hub with no registered sources.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
            OverflowPolicy::LatestOnly => {
                let channel = Arc::new(LatestChannel::new());
                (
                    Channel::LatestOnly(Arc::clone(&channel)),
                    SourceHandle::LatestOnly(LatestSender { inner: channel }),
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
            Channel::LatestOnly(channel) => Subscription::LatestOnly(LatestReceiver {
                inner: Arc::clone(channel),
                rx: channel.tx.subscribe(),
            }),
            Channel::UnboundedFlagged(channel) => {
                Subscription::UnboundedFlagged(UnboundedReceiver {
                    inner: Arc::clone(channel),
                })
            }
        })
    }

    /// Register the process's one head source as a `NewHead` /
    /// [`OverflowPolicy::LatestOnly`] channel.
    ///
    /// The hub owns the latest head and its staleness clock; the transport
    /// (e.g. `degenbot_rpc::head_watch`) publishes every observed header and
    /// keeps no head state of its own.
    ///
    /// # Errors
    ///
    /// [`HubError::AlreadyRegistered`] if a `NewHead` source already exists on
    /// this hub.
    pub fn register_head_source(&self) -> Result<HeadSender, HubError> {
        match self.register_source(HubClass::NewHead, OverflowPolicy::LatestOnly, 0)? {
            SourceHandle::LatestOnly(sender) => Ok(HeadSender::new(sender)),
            _ => Err(HubError::PolicyMismatch {
                expected: "LatestOnly",
            }),
        }
    }

    /// Subscribe to the hub head source.
    ///
    /// # Errors
    ///
    /// [`HubError::NotRegistered`] if [`Self::register_head_source`] has not
    /// run on this hub.
    pub fn subscribe_head(&self) -> Result<HeadSubscription, HubError> {
        match self.subscribe(HubClass::NewHead)? {
            Subscription::LatestOnly(receiver) => Ok(HeadSubscription::new(receiver)),
            _ => Err(HubError::PolicyMismatch {
                expected: "LatestOnly",
            }),
        }
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

    /// Count of registrations using the deliberate unbounded audit posture.
    #[must_use]
    pub fn unbounded_flagged_count(&self) -> usize {
        self.sources
            .lock()
            .values()
            .filter(|entry| entry.policy.is_unbounded_flagged())
            .count()
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
    use crate::event::PendingTx;

    fn tx(nonce: u64) -> PendingTx {
        PendingTx {
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
            HubEvent::NewHead { .. } | HubEvent::PoolEvent { .. } => None,
        }
    }

    fn drop_oldest_sender(hub: &Hub, capacity: usize) -> DropOldestSender {
        match hub
            .register_source(
                HubClass::PendingTx,
                OverflowPolicy::DropOldestCounted {
                    name: "dropped_ring",
                },
                capacity,
            )
            .expect("registers")
        {
            SourceHandle::DropOldestCounted(sender) => sender,
            _ => panic!("declared drop-oldest, got another handle"),
        }
    }

    #[test]
    fn policy_is_fixed_at_registration() {
        let hub = Hub::new();
        assert!(matches!(
            hub.subscribe(HubClass::PendingTx),
            Err(HubError::NotRegistered(HubClass::PendingTx))
        ));
        let handle = drop_oldest_sender(&hub, 4);
        assert_eq!(handle.name(), "dropped_ring");
        assert!(matches!(
            hub.register_source(HubClass::PendingTx, OverflowPolicy::LatestOnly, 4),
            Err(HubError::AlreadyRegistered(HubClass::PendingTx))
        ));
        assert!(matches!(
            hub.subscribe(HubClass::PendingTx),
            Ok(Subscription::DropOldestCounted(_))
        ));
        assert_eq!(
            hub.policy_of(HubClass::PendingTx),
            Some(OverflowPolicy::DropOldestCounted {
                name: "dropped_ring"
            })
        );
        assert_eq!(hub.registered_classes(), vec![HubClass::PendingTx]);
        assert_eq!(hub.unbounded_flagged_count(), 0);
    }

    #[test]
    fn drop_oldest_counted_evicts_oldest_and_counts() {
        let hub = Hub::new();
        let sender = drop_oldest_sender(&hub, 4);
        let Subscription::DropOldestCounted(receiver) =
            hub.subscribe(HubClass::PendingTx).expect("subscribed")
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

    fn new_head(number: u64) -> HubEvent {
        HubEvent::NewHead {
            number,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        }
    }

    #[tokio::test]
    async fn head_advance_and_stale_falls_back_to_poll() {
        use std::time::Duration;

        let hub = Hub::new();
        let sender = hub.register_head_source().expect("registers");
        assert_eq!(
            hub.policy_of(HubClass::NewHead),
            Some(OverflowPolicy::LatestOnly),
            "the head source declares the LatestOnly policy"
        );
        let mut head = hub.subscribe_head().expect("subscribed");
        assert_eq!(head.head(), None, "a fresh head source has no head");
        assert!(
            !head.stale(Duration::from_secs(30)),
            "a just-registered source is not stale"
        );

        // LatestOnly: only the newest head survives.
        sender.publish(new_head(10));
        sender.publish(new_head(11));
        assert_eq!(
            head.borrow_and_update(),
            Some(11),
            "superseded heads are dropped"
        );

        // The consumer awaits the next advance, then reads it.
        let waiter = tokio::spawn(async move {
            head.changed().await.expect("sender stays alive");
            head.borrow_and_update()
        });
        sender.publish(new_head(12));
        assert_eq!(waiter.await.expect("join"), Some(12));

        // The stale->poll-fallback predicate: a quiet source past the
        // threshold reports stale, which is exactly what makes the live loop
        // poll `eth_blockNumber` for one iteration.
        let probe = hub.subscribe_head().expect("subscribed");
        assert!(!probe.stale(Duration::from_secs(30)), "just published");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            probe.stale(Duration::from_millis(10)),
            "quiet past the threshold is stale"
        );
    }

    #[test]
    fn latest_only_keeps_only_the_last() {
        let hub = Hub::new();
        let handle = hub
            .register_source(HubClass::NewHead, OverflowPolicy::LatestOnly, 0)
            .expect("registers");
        let SourceHandle::LatestOnly(sender) = handle else {
            panic!("declared latest-only, got another handle");
        };
        let Subscription::LatestOnly(receiver) =
            hub.subscribe(HubClass::NewHead).expect("subscribed")
        else {
            panic!("policy changed under test");
        };
        assert!(receiver.latest().is_none());
        for nonce in 0..5 {
            sender.push(HubEvent::PendingTx(tx(nonce)));
        }
        assert_eq!(receiver.latest().and_then(nonce_of), Some(4));
        assert_eq!(
            hub.policy_of(HubClass::NewHead),
            Some(OverflowPolicy::LatestOnly)
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
}
