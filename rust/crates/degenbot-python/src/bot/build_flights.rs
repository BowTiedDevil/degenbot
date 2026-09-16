//! Engine-internal single-flight pool builds (PRG-1 registry
//! unification).
//!
//! Replaces the CXKACI FFI claim table (`pool_build_claims.rs`, a `PyO3`
//! peer driven by a Python claim wrapper): the propagation seam between the
//! Python crawl shell and the Rust build is DISSOLVED — `BotState` is the sole
//! pool registry of record, so the coordination moves inside the build path
//! itself:
//!
//! - the caller that arrives first **leads** the build and publishes the
//!   registered pool id strictly AFTER its `BotState` registration;
//! - concurrent peers for the same `(family, chain_id, key)` park GIL-free
//!   on a condvar and wake with the published result (built pool id, or the
//!   leader's exact failure — same exception class, so the driver's fatal
//!   classification is unchanged);
//! - a peer whose claim window closed before it subscribed finds no claim in
//!   the table and re-runs the pre-check/claim loop — the `BotState`
//!   registry-of-record pre-check answers.
//!
//! One seat per `(family, key)` — the same keyed-unit admission semantics
//! the hot-solver pin fix (4456e78f9) established for the solve fleet.
//!
//! Waiting parks inside `Python::detach` (the caller releases the GIL for
//! the whole park — incident 2026-08-20 #2's inversion class again: a GIL-
//! holding condvar park could never be woken by a detached leader).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use pyo3::exceptions::PyBaseException;
use pyo3::prelude::*;

/// One build-flight identity: (family, `chain_id`, `key`) where `key` is the
/// lowercase pool address (V2/V3/Aerodrome/Balancer) or the
/// `{pool_manager-lower}:{pool_id-hex}` string (V4). Mirrors the identity
/// the pool registries key on.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FlightKey(pub(crate) u8, pub(crate) u64, pub(crate) String);

/// Family tags for `FlightKey`.0 — u8 discriminants keep the key cheap.
pub(crate) mod flight_family {
    pub(crate) const V2: u8 = b'2';
    pub(crate) const V3: u8 = b'3';
    pub(crate) const V4: u8 = b'4';
    pub(crate) const AERODROME: u8 = b'A';
    pub(crate) const BALANCER_WEIGHTED: u8 = b'W';
    pub(crate) const BALANCER_STABLE: u8 = b'S';
}

/// Slot state inside one in-flight (or just-published) build.
enum SlotState {
    /// A leader owns the build.
    Building,
    /// The leader registered the pool (id) into `BotState` and published
    /// AFTER that insertion — the publication contract of PRG-1.
    Built(u64),
    /// The leader published its failure. The exact exception VALUE the
    /// leader would have raised (same class/carrier) propagates to every
    /// waiter, so driver-side classification (e.g. V4 fatal admission) is
    /// unchanged. Stored as the normalized exception object (GIL-free
    /// re-borrow on wake is impossible for a `PyErr`, which is un-cloneable
    /// in pyo3 0.29 while detached).
    Failed(Box<Py<PyBaseException>>),
}

/// One in-flight build. Waiters hold an `Arc` clone, so publication + map
/// removal never strands a parked subscriber (the state stays readable).
pub(crate) struct FlightSlot {
    state: Mutex<SlotState>,
    cv: Condvar,
}

impl FlightSlot {
    /// Park GIL-free until the leader publishes. `Building` → `Built`/
    /// `Failed` via the condvar; the parked loop re-checks the retained
    /// state on every wake (spurious-wake safe). Once published the state is
    /// final (the leader is done and the slot is removed from the table), so
    /// the GIL-held failure clone after `detach` returns is race-free.
    pub(crate) fn wait(self: &Arc<Self>, py: Python<'_>) -> Parked {
        let slot = Arc::clone(self);
        let signal = py.detach(move || {
            let mut state = slot.state.lock();
            loop {
                match &*state {
                    SlotState::Building => {
                        slot.cv.wait(&mut state);
                    }
                    SlotState::Built(pool_id) => return ParkedSignal::Built(*pool_id),
                    SlotState::Failed(_) => return ParkedSignal::Failed,
                }
            }
        });
        match signal {
            ParkedSignal::Built(pool_id) => Parked::Built(pool_id),
            ParkedSignal::Failed => {
                // GIL held here — clone the published exception value.
                let state = self.state.lock();
                match &*state {
                    SlotState::Failed(err) => Parked::Failed((**err).clone_ref(py)),
                    SlotState::Built(id) => Parked::Built(*id),
                    SlotState::Building => unreachable!("parked past publication"),
                }
            }
        }
    }
}

/// The parked-wait result.
pub(crate) enum Parked {
    /// The leader published a registered pool id ("strictly after" the
    /// `BotState` insertion, so a `Built` wake never precedes the registry).
    /// The id itself is informational (tests assert it); consumers
    /// re-answer through the registry-of-record pre-check.
    Built(#[cfg_attr(not(test), expect(dead_code))] u64),
    /// The leader's exact failure — the exception object it raised.
    Failed(Py<PyBaseException>),
}

/// The detached-park signal (`Building` loop exit) preceding a GIL-held
/// clone of the failure object.
enum ParkedSignal {
    Built(u64),
    Failed,
}

/// Outcome of trying to enter the flight for a key.
pub(crate) enum FlightEnter<'f> {
    /// The caller leads the build; it MUST publish exactly once (built id or
    /// failure) — the guard's `Drop` publishes a loud failure otherwise. The
    /// guard borrows the table, so the lead cannot outlive `BuildFlights`.
    Lead(FlightsGuard<'f>),
    /// A build is in flight; park with `FlightSlot::wait`.
    Peer(Arc<FlightSlot>),
}

/// `PyBot`-internal single-flight build table (PRG-1). One per `PyBot`.
#[derive(Default)]
pub(crate) struct BuildFlights {
    flights: Mutex<HashMap<FlightKey, Arc<FlightSlot>>>,
}

impl BuildFlights {
    /// Try to claim the build for `key`. `Lead` when this caller won the
    /// seat; `Peer` when a build is in flight.
    pub(crate) fn enter(&self, key: &FlightKey) -> FlightEnter<'_> {
        let mut map = self.flights.lock();
        if let Some(slot) = map.get(key).cloned() {
            return FlightEnter::Peer(slot);
        }
        map.insert(
            key.clone(),
            Arc::new(FlightSlot {
                state: Mutex::new(SlotState::Building),
                cv: Condvar::new(),
            }),
        );
        FlightEnter::Lead(FlightsGuard {
            flights: self,
            key: key.clone(),
            done: false,
        })
    }

    /// Publish the built pool id (leader only, strictly after the `BotState`
    /// registration) and release the claim. Parked waiters wake with the id.
    fn publish_built(&self, key: &FlightKey, pool_id: u64) {
        self.publish(key, SlotState::Built(pool_id));
    }

    /// Publish a build failure (leader only) and release the claim. Parked
    /// waiters raise the same error; a later candidate builds fresh (the
    /// registry-of-record pre-check answers).
    fn publish_failed(&self, py: Python<'_>, key: &FlightKey, err: &PyErr) {
        // The normalized exception value (same class/carrier the leader
        // raised) is what waiters re-raise.
        let value = err.value(py).clone().unbind();
        self.publish(key, SlotState::Failed(Box::new(value)));
    }

    fn publish(&self, key: &FlightKey, state: SlotState) {
        let slot = {
            let mut map = self.flights.lock();
            map.remove(key)
        };
        if let Some(slot) = slot {
            // State update + wake under the slot's own lock; waiters hold
            // `Arc` clones so the map removal above cannot strand them.
            let mut guard = slot.state.lock();
            *guard = state;
            slot.cv.notify_all();
        }
    }
}

/// Leader-side guard: publishes exactly once (or loudly fails on leak).
pub(crate) struct FlightsGuard<'f> {
    flights: &'f BuildFlights,
    key: FlightKey,
    done: bool,
}

impl FlightsGuard<'_> {
    /// Publish the built pool id (leader only, strictly after the `BotState`
    /// registration) and release the claim.
    pub(crate) fn publish_built(mut self, pool_id: u64) {
        self.done = true;
        self.flights.publish_built(&self.key, pool_id);
    }

    /// Publish a build failure (leader only) and release the claim.
    pub(crate) fn publish_failed(mut self, py: Python<'_>, err: &PyErr) {
        self.done = true;
        self.flights.publish_failed(py, &self.key, err);
    }
}

impl Drop for FlightsGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            // GIL-free attach: drop usually runs with the GIL held (the
            // pymethod outcome handling); attach is re-entrant.
            Python::attach(|py| {
                let err = pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "build flight dropped unpublished (key {}) — leader bug",
                    self.key.2,
                ));
                self.flights.publish_failed(py, &self.key, &err);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions fail loudly"
    )]

    use super::*;

    fn key(family: u8, s: &str) -> FlightKey {
        FlightKey(family, 1, s.to_string())
    }

    #[test]
    fn first_claimant_leads_second_is_peer() {
        let flights = BuildFlights::default();
        let key = key(flight_family::V2, "pool-a");
        // BIND the guard (a `matches!` drop publishes and releases the
        // claim — that is the guard's loud-failure contract, not a bug).
        let FlightEnter::Lead(_lead) = flights.enter(&key) else {
            panic!("first claimant must lead");
        };
        assert!(
            matches!(flights.enter(&key), FlightEnter::Peer(_)),
            "second claimant must see the in-flight peer"
        );
    }

    #[test]
    fn publication_releases_the_claim_and_wakes_parked_waiters() {
        let flights = std::sync::Arc::new(BuildFlights::default());
        let key = key(flight_family::V4, "pm:0xdead");
        let FlightEnter::Lead(_lead) = flights.enter(&key) else {
            panic!("test setup: claim");
        };
        let FlightEnter::Peer(slot) = flights.enter(&key) else {
            panic!("expected peer")
        };
        // Publication while a waiter is parked — drive the publisher on a
        // thread.
        let key2 = key.clone();
        let flights2 = std::sync::Arc::clone(&flights);
        let publisher = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            flights2.publish_built(&key2, 777);
            flights2.flights.lock().remove(&key2);
        });
        let parked = Python::attach(|py| slot.wait(py));
        let Parked::Built(pool_id) = parked else {
            panic!("waiter must wake with the built id")
        };
        assert_eq!(pool_id, 777);
        publisher.join().expect("publisher thread");
        // The slot is released: the next caller leads again (a benign
        // redundant build — the registry-of-record pre-check answers).
        assert!(matches!(flights.enter(&key), FlightEnter::Lead(_)));
    }

    #[test]
    fn failure_publishes_the_exact_error_to_waiters() {
        Python::attach(|py| {
            let flights = std::sync::Arc::new(BuildFlights::default());
            let key = key(flight_family::V3, "pool-b");
            let FlightEnter::Lead(_lead) = flights.enter(&key) else {
                panic!("test setup: claim");
            };
            let FlightEnter::Peer(slot) = flights.enter(&key) else {
                panic!("expected peer")
            };
            let key2 = key.clone();
            let flights2 = std::sync::Arc::clone(&flights);
            let publisher = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                Python::attach(|py2| {
                    let err = pyo3::exceptions::PyValueError::new_err("boom");
                    flights2.publish_failed(py2, &key2, &err);
                });
                flights2.flights.lock().remove(&key2);
            });
            let Parked::Failed(err_obj) = slot.wait(py) else {
                panic!("expected the leader's failure")
            };
            publisher.join().expect("publisher thread");
            let err = PyErr::from_value(err_obj.into_bound(py).into_any());
            assert!(
                err.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                "the waiter sees the leader's exact exception class"
            );
        });
    }

    #[test]
    fn dropped_guard_releases_the_claim() {
        let flights = BuildFlights::default();
        let key = key(flight_family::AERODROME, "pool-c");
        drop(flights.enter(&key));
        // The Drop published a (loud) failure and removed the slot, so the
        // next claimant LEADS again — a claim can never wedge shut.
        assert!(matches!(flights.enter(&key), FlightEnter::Lead(_)));
    }
}
