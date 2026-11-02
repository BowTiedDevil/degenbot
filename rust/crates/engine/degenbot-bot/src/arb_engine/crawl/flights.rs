//! The crawl's single-flight build table — concurrent same-pool builds share
//! ONE construction.
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
//! - a peer parks asynchronously and wakes with the leader's answer, or with
//!   the leader's exact [`ConstructionRefusal`] value, so its own
//!   classification (stable skip / loud abort) is the leader's;
//! - a leader that never publishes — a panic or a cancelled future — publishes
//!   a loud transient failure from its guard's `Drop`, so a peer can never park
//!   forever on a dead lead.
//!
//! This is the pyo3-free core twin of the Python shell's `BuildFlights`
//! (`degenbot-python/src/bot/build_flights.rs`): the key is typed here, the
//! wait is an `await` rather than a GIL-released condvar park, and the payload
//! is a typed refusal rather than a `PyBaseException`. The shell's table keeps
//! its own copy until the Python registration path is retired.

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlightOutcome {
    /// The pool is registered under this id.
    Built(u64),
    /// The construction was refused — the leader's own typed refusal, so a peer
    /// classifies it exactly as the leader did.
    Failed(ConstructionRefusal),
}

/// The state one flight publishes.
enum SlotState {
    /// A leader owns the build.
    Building,
    /// The leader finished (published strictly after the state registration).
    Answered(FlightOutcome),
}

/// One retired flight's retained slot — a peer that subscribed before the
/// leader published holds an `Arc` clone, so publication and table removal can
/// never strand it.
struct FlightSlot {
    state: Mutex<SlotState>,
    published: Notify,
}

impl FlightSlot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SlotState::Building),
            published: Notify::new(),
        })
    }

    /// Park until the leader publishes.
    ///
    /// The notification future is registered *before* the state is re-checked
    /// (`Notify::notified` + `enable`), so a publication between the check and
    /// the await cannot be lost, and a spurious wake re-checks the retained
    /// state rather than assuming an answer.
    async fn wait(&self) -> FlightOutcome {
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
pub enum FlightEnter<'a> {
    /// The caller leads the build and must publish exactly once (§ the guard's
    /// `Drop` publishes a loud transient failure otherwise).
    Lead(FlightGuard<'a>),
    /// A build is already in flight; park with [`FlightSlot::wait`].
    Peer(FlightSlotHandle),
}

/// A peer's handle on an in-flight build.
pub struct FlightSlotHandle(Arc<FlightSlot>);

impl FlightSlotHandle {
    /// Park until the leader publishes (see [`FlightSlot::wait`]).
    pub async fn wait(&self) -> FlightOutcome {
        self.0.wait().await
    }
}

/// The crawl's build-flight table. One per crawl; shared across seats.
#[derive(Default)]
pub struct BuildFlights {
    slots: Mutex<HashMap<FlightKey, Arc<FlightSlot>>>,
}

impl BuildFlights {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enter the flight for `key` — lead it, or park on the leader.
    pub fn enter(&self, key: FlightKey) -> FlightEnter<'_> {
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

    /// How many flights are live (test/diagnostic witness).
    #[cfg(test)]
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.slots.lock().len()
    }
}

/// The leader's publish token. Publishes once; dropping it unpublished
/// publishes a loud transient failure so no peer parks on a dead lead.
pub struct FlightGuard<'a> {
    flights: &'a BuildFlights,
    key: FlightKey,
    slot: Arc<FlightSlot>,
    published: bool,
}

impl FlightGuard<'_> {
    /// Publish the leader's answer and wake every parked peer.
    ///
    /// A second call is a no-op: the flight's answer is final (the leader built
    /// once, and a correction would be a second build the registry would have
    /// to fold).
    pub fn publish(&mut self, outcome: FlightOutcome) {
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

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // The lead ended without an answer — a panic, or a cancelled future.
        // A peer must not park forever, and a transient refusal is the honest
        // classification: nothing about the pool was decided.
        self.publish(FlightOutcome::Failed(ConstructionRefusal::Skipped(
            degenbot_pools::BuildFailure::Transient(
                "the build-flight leader ended without publishing an answer".to_owned(),
            ),
        )));
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "tests assert on known-valid fixtures")]
mod tests {
    use alloy::primitives::address;
    use degenbot_pools::BuildFailure;

    use super::*;

    const POOL: Address = address!("2222222222222222222222222222222222222222");

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
    async fn a_leader_that_never_publishes_still_wakes_its_peers() {
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
