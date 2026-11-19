//! The build-flight table — concurrent builds of one pool share ONE
//! construction, in both drivers.
//!
//! The build's *correctness* is already core: [`Bot::construct_pool`]'s
//! get-or-register answers a registered pool without I/O and folds the
//! `AlreadyRegistered` race back to the winner's pool id. What a crawl without
//! flights re-pays is the *cost*: N seats that discover the same unregistered
//! pool at once each run the full construction choreography, and all but one
//! throw the work away.
//!
//! A flight makes one caller the leader for a `(family, chain, identity)` key
//! and parks the peers on the leader's answer:
//!
//! - the leader builds, then publishes the registered pool id **strictly
//!   after** the state registration (the publication contract — a woken peer
//!   never reads an id the registry does not hold);
//! - a peer parks and wakes with the leader's answer, or with the leader's own
//!   failure payload, so its classification (stable skip / loud abort, exact
//!   exception class) is the leader's;
//! - a leader that never publishes — a panic or a cancelled future — publishes
//!   the table's lead-lost answer from its guard's `Drop`, so a peer can never
//!   park forever on a dead lead.
//!
//! One table serves both drivers. [`BuildFlights<T>`] is generic over the
//! failure payload `T` the driver publishes: the crawl publishes a typed
//! [`ConstructionRefusal`] ([`CrawlFlights`]), the `PyO3` driver publishes the
//! leader's exact exception value behind an `Arc` (an `Arc` clone needs no GIL,
//! a `Py` clone does). Key, election, wait and drop are the same code in both.

use alloy::primitives::Address;
use degenbot_pathfinding::PoolKind;
use hashbrown::HashMap;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::bot_core::ConstructionRefusal;

/// One build-flight identity: the family, the chain, and the identity the pool
/// registers under (its contract address, or a V4 pool's `(PoolManager,
/// pool_id)` pair — one manager hosts many pools).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlightKey {
    /// The family the edge named.
    pub kind: PoolKind,
    /// The chain the pool lives on.
    pub chain_id: u64,
    /// The registered identity.
    pub target: FlightTarget,
}

/// The identity half of a [`FlightKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlightTarget {
    /// A contract-address family (V2, V3).
    Address(Address),
    /// A V4 pool, by manager and on-chain id.
    V4 {
        /// The canonical `PoolManager`.
        pool_manager: Address,
        /// The pool's on-chain id.
        pool_id: [u8; 32],
    },
}

/// The leader's published answer.
///
/// `T` is the driver's failure payload — the core's typed
/// [`ConstructionRefusal`], the `PyO3` driver's exact exception value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlightOutcome<T> {
    /// The pool is registered under this id.
    Built(u64),
    /// The construction was refused — the leader's own failure, so a peer
    /// classifies it exactly as the leader did.
    Failed(T),
}

/// The state one flight publishes.
enum SlotState<T> {
    /// A leader owns the build.
    Building,
    /// The leader finished (published strictly after the state registration).
    Answered(FlightOutcome<T>),
}

/// One retired flight's retained slot — a peer that subscribed before the
/// leader published holds an `Arc` clone, so publication and table removal can
/// never strand it.
struct FlightSlot<T> {
    state: Mutex<SlotState<T>>,
    published: Notify,
}

impl<T> FlightSlot<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SlotState::Building),
            published: Notify::new(),
        })
    }
}

impl<T: Clone> FlightSlot<T> {
    /// Park until the leader publishes.
    ///
    /// The notification future is registered *before* the state is re-checked
    /// (`Notify::notified` + `enable`), so a publication between the check and
    /// the await cannot be lost, and a spurious wake re-checks the retained
    /// state rather than assuming an answer.
    async fn wait(&self) -> FlightOutcome<T> {
        loop {
            let published = self.published.notified();
            tokio::pin!(published);
            published.as_mut().enable();
            {
                let state = self.state.lock();
                if let SlotState::Answered(outcome) = &*state {
                    return outcome.clone();
                }
            }
            published.await;
        }
    }
}

/// What entering a flight answers.
pub enum FlightEnter<'a, T> {
    /// The caller leads the build and must publish exactly once (§ the guard's
    /// `Drop` publishes the lead-lost answer otherwise).
    Lead(FlightGuard<'a, T>),
    /// A build is already in flight; park with [`FlightSlotHandle::wait`].
    Peer(FlightSlotHandle<T>),
}

/// A peer's handle on an in-flight build.
pub struct FlightSlotHandle<T>(Arc<FlightSlot<T>>);

impl<T: Clone> FlightSlotHandle<T> {
    /// Park until the leader publishes (see [`FlightSlot::wait`]).
    pub async fn wait(&self) -> FlightOutcome<T> {
        self.0.wait().await
    }
}

/// The build-flight table. One per driver; shared across its seats.
///
/// `lead_lost` renders the answer a peer receives when its leader ended without
/// publishing (a panic, a cancelled future): the driver owns that rendering, so
/// the failure vocabulary never leaks across the boundary.
pub struct BuildFlights<T> {
    slots: Mutex<HashMap<FlightKey, Arc<FlightSlot<T>>>>,
    lead_lost: fn(FlightKey) -> T,
}

impl<T> BuildFlights<T> {
    /// An empty table whose abandoned leads answer `lead_lost(key)`.
    #[must_use]
    pub fn with_lead_lost(lead_lost: fn(FlightKey) -> T) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            lead_lost,
        }
    }

    /// Enter the flight for `key` — lead it, or park on the leader.
    pub fn enter(&self, key: FlightKey) -> FlightEnter<'_, T> {
        let mut slots = self.slots.lock();
        if let Some(slot) = slots.get(&key) {
            return FlightEnter::Peer(FlightSlotHandle(Arc::clone(slot)));
        }
        let slot = FlightSlot::new();
        slots.insert(key, Arc::clone(&slot));
        FlightEnter::Lead(FlightGuard {
            flights: self,
            key,
            slot,
            published: false,
        })
    }
}

impl BuildFlights<ConstructionRefusal> {
    /// The crawl's table: an abandoned lead is a loud transient refusal —
    /// nothing about the pool was decided.
    #[must_use]
    pub fn new() -> Self {
        Self::with_lead_lost(lead_lost_refusal)
    }

    /// How many flights are live (test/diagnostic witness).
    #[cfg(test)]
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.slots.lock().len()
    }
}

impl Default for BuildFlights<ConstructionRefusal> {
    fn default() -> Self {
        Self::new()
    }
}

/// The crawl's [`BuildFlights`]: its failure payload is the core's typed
/// refusal, so a peer's classification is the leader's.
pub type CrawlFlights = BuildFlights<ConstructionRefusal>;

fn lead_lost_refusal(_: FlightKey) -> ConstructionRefusal {
    ConstructionRefusal::Skipped(degenbot_pools::BuildFailure::Transient(
        "the build-flight leader ended without publishing an answer".to_owned(),
    ))
}

/// The leader's publish token. Publishes once; dropping it unpublished
/// publishes the table's lead-lost answer so no peer parks on a dead lead.
pub struct FlightGuard<'a, T> {
    flights: &'a BuildFlights<T>,
    key: FlightKey,
    slot: Arc<FlightSlot<T>>,
    published: bool,
}

impl<T> FlightGuard<'_, T> {
    /// Publish the leader's answer and wake every parked peer.
    ///
    /// A second call is a no-op: the flight's answer is final (the leader built
    /// once, and a correction would be a second build the registry would have
    /// to fold).
    pub fn publish(&mut self, outcome: FlightOutcome<T>) {
        if self.published {
            return;
        }
        self.published = true;
        {
            let mut state = self.slot.state.lock();
            *state = SlotState::Answered(outcome);
        }
        self.slot.published.notify_waiters();
        let mut slots = self.flights.slots.lock();
        if slots
            .get(&self.key)
            .is_some_and(|live| Arc::ptr_eq(live, &self.slot))
        {
            slots.remove(&self.key);
        }
    }
}

impl<T> Drop for FlightGuard<'_, T> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // The lead ended without an answer — a panic, or a cancelled future.
        // A peer must not park forever, and the driver's lead-lost answer is
        // the honest classification: nothing about the pool was decided.
        let answer = (self.flights.lead_lost)(self.key);
        self.publish(FlightOutcome::Failed(answer));
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests assert on known-valid fixtures")]
mod tests {
    use alloy::primitives::address;
    use degenbot_pools::BuildFailure;
    use tokio::sync::Notify;

    use super::*;

    const POOL: Address = address!("2222222222222222222222222222222222222222");
    const OTHER: Address = address!("3333333333333333333333333333333333333333");

    fn key() -> FlightKey {
        FlightKey {
            kind: PoolKind::V3,
            chain_id: 1,
            target: FlightTarget::Address(POOL),
        }
    }

    fn transient() -> ConstructionRefusal {
        ConstructionRefusal::Skipped(BuildFailure::Transient("leader failed".to_owned()))
    }

    #[tokio::test]
    async fn a_peer_rides_the_leaders_registered_pool_id() {
        let flights = BuildFlights::new();
        let FlightEnter::Lead(mut lead) = flights.enter(key()) else {
            panic!("the first entrant leads");
        };
        let FlightEnter::Peer(peer) = flights.enter(key()) else {
            panic!("a concurrent entrant parks");
        };
        assert_eq!(flights.live_count(), 1);
        lead.publish(FlightOutcome::Built(7));
        assert_eq!(peer.wait().await, FlightOutcome::Built(7));
        assert_eq!(
            flights.live_count(),
            0,
            "a published flight retires from the table"
        );
        assert!(
            matches!(flights.enter(key()), FlightEnter::Lead(_)),
            "the next entrant after publication leads a new flight"
        );
    }

    #[tokio::test]
    async fn a_peer_receives_the_leaders_exact_refusal() {
        let flights = BuildFlights::new();
        let FlightEnter::Lead(mut lead) = flights.enter(key()) else {
            panic!("the first entrant leads");
        };
        let FlightEnter::Peer(peer) = flights.enter(key()) else {
            panic!("a concurrent entrant parks");
        };
        lead.publish(FlightOutcome::Failed(transient()));
        assert_eq!(peer.wait().await, FlightOutcome::Failed(transient()));
    }

    #[tokio::test]
    async fn a_cancelled_leader_still_wakes_its_peers() {
        let flights = BuildFlights::new();
        let (peer, lead) = {
            let FlightEnter::Lead(lead) = flights.enter(key()) else {
                panic!("the first entrant leads");
            };
            let FlightEnter::Peer(peer) = flights.enter(key()) else {
                panic!("a concurrent entrant parks");
            };
            (peer, lead)
        };
        // The lead ends without an answer — a panic or a cancelled future.
        drop(lead);
        match peer.wait().await {
            FlightOutcome::Failed(ConstructionRefusal::Skipped(BuildFailure::Transient(_))) => {}
            other => panic!("a dead lead must publish a transient failure, got {other:?}"),
        }
        assert_eq!(flights.live_count(), 0);
    }

    #[tokio::test]
    async fn concurrent_seats_share_one_construction() {
        let flights = BuildFlights::new();
        // The leader holds the flight open until both seats have entered, so
        // the peer parks on a live flight rather than finding a retired slot.
        let gate = Notify::new();
        let leader = async {
            let FlightEnter::Lead(mut lead) = flights.enter(key()) else {
                panic!("the first entrant leads");
            };
            gate.notified().await;
            lead.publish(FlightOutcome::Built(11));
            11
        };
        let peer = async {
            let FlightEnter::Peer(peer) = flights.enter(key()) else {
                panic!("the concurrent entrant parks");
            };
            gate.notify_one();
            match peer.wait().await {
                FlightOutcome::Built(pool_id) => pool_id,
                FlightOutcome::Failed(refusal) => panic!("the leader built, got {refusal:?}"),
            }
        };
        let (led, ridden) = tokio::join!(leader, peer);
        assert_eq!(led, ridden, "the peer rides the leader's own construction");
        assert_eq!(flights.live_count(), 0);
    }

    #[tokio::test]
    async fn distinct_identities_do_not_share_a_flight() {
        let flights = BuildFlights::new();
        let FlightEnter::Lead(_lead) = flights.enter(key()) else {
            panic!("the first entrant leads");
        };
        let mut leads = Vec::new();
        for other in [
            FlightKey {
                target: FlightTarget::Address(OTHER),
                ..key()
            },
            FlightKey {
                chain_id: 2,
                ..key()
            },
            FlightKey {
                kind: PoolKind::V2,
                ..key()
            },
        ] {
            let FlightEnter::Lead(lead) = flights.enter(other) else {
                panic!("a different identity leads its own flight: {other:?}");
            };
            leads.push(lead);
        }
        assert_eq!(flights.live_count(), 4);
    }

    #[tokio::test]
    async fn the_table_answers_an_abandoned_lead_with_its_own_failure_payload() {
        // The generic contract: the driver owns the lead-lost rendering, so a
        // non-refusal payload crosses the same state machine.
        let flights = BuildFlights::with_lead_lost(|key: FlightKey| format!("lost {key:?}"));
        let (peer, lead) = {
            let FlightEnter::Lead(lead) = flights.enter(key()) else {
                panic!("the first entrant leads");
            };
            let FlightEnter::Peer(peer) = flights.enter(key()) else {
                panic!("a concurrent entrant parks");
            };
            (peer, lead)
        };
        drop(lead);
        match peer.wait().await {
            FlightOutcome::Failed(answer) => assert!(answer.starts_with("lost FlightKey")),
            other @ FlightOutcome::Built(_) => {
                panic!("the table's lead-lost answer must reach the peer, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn a_second_publish_is_a_no_op() {
        let flights = BuildFlights::new();
        let FlightEnter::Lead(mut lead) = flights.enter(key()) else {
            panic!("the first entrant leads");
        };
        lead.publish(FlightOutcome::Built(1));
        lead.publish(FlightOutcome::Built(2));
        assert!(
            matches!(flights.enter(key()), FlightEnter::Lead(_)),
            "the first publish retired the flight, so the table is clean"
        );
    }
}
