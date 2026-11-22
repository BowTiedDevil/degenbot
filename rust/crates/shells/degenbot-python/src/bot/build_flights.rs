//! The `PyO3` driver's side of the ONE build-flight protocol.
//!
//! The table itself is the core's (`degenbot_bot::crawl::BuildFlights`): the
//! key, the leader election, the `await`, and the abandoned-lead contract are
//! one implementation, shared with the async Rust crawl. This module carries
//! the things only the `PyO3` driver can supply:
//!
//! - the failure payload: the leader's exact exception value behind an `Arc`,
//!   because an `Arc` clone needs no GIL while a `Py` clone does — a peer
//!   re-raises the value only once it holds the GIL again;
//! - the flight identity: a translated [`PoolConstruction`] already names the
//!   registered identity (its address, or a V4 `(PoolManager, pool_id)` pair),
//!   so the key is read off the construction the build consumes rather than
//!   re-derived from the request;
//! - the protocol arc ([`run`]): the leader builds under its claim, peers park
//!   on the answer, and a caller that keeps losing the election refuses with a
//!   typed claim-exhaustion error instead of building outside a claim.
//!
//! Waiting parks GIL-free (`Python::detach`) on the core slot's async wait,
//! driven on the shared runtime: a parked peer holds no GIL, so the detached
//! leader (or any other thread) can build, register, and publish.

use std::sync::Arc;

use degenbot_bot::bot_core::PoolConstruction;
use degenbot_bot::crawl::{
    BuildFlights, FlightEnter, FlightKey, FlightOutcome, FlightSlotHandle, FlightTarget,
};
use degenbot_pathfinding::PoolKind;
use pyo3::exceptions::{PyBaseException, PyRuntimeError};
use pyo3::prelude::*;

/// The leader's failure as it crosses the boundary: its exact exception value.
///
/// `Arc` (not a bare `Py`) so the value's clone — a publication, a table
/// removal, a second peer — never needs the GIL.
pub(crate) type PythonFlightFailure = Arc<Py<PyBaseException>>;

/// The flight answer the driver consumes.
pub(crate) type PythonFlightOutcome = FlightOutcome<PythonFlightFailure>;

/// The bot's table, holding Python failure payloads.
pub(crate) type PythonBuildFlights = BuildFlights<PythonFlightFailure>;

/// A fresh table for one driver.
pub(crate) fn table() -> PythonBuildFlights {
    BuildFlights::with_lead_lost(lead_lost)
}

/// The flight identity of a translated construction request.
///
/// The construction IS the family authority: every address-keyed family binds
/// its pool to the one `PoolLookup::Address` and shares the core's
/// address-keyed `V2` class (the graph's `V2` discriminant covers Aerodrome
/// pairs too); the V4 family is the one identity that is not an address, so it
/// keys on its `(PoolManager, pool_id)` pair.
pub(crate) fn flight_key(construction: &PoolConstruction<'_>, chain_id: u64) -> FlightKey {
    let (kind, target) = match construction {
        PoolConstruction::V2 { address, .. }
        | PoolConstruction::AerodromeV2 { address, .. }
        | PoolConstruction::Curve { address, .. }
        | PoolConstruction::BalancerWeighted { address, .. }
        | PoolConstruction::BalancerStable { address, .. } => {
            (PoolKind::V2, FlightTarget::Address(*address))
        }
        PoolConstruction::V3 { address, .. } => (PoolKind::V3, FlightTarget::Address(*address)),
        PoolConstruction::V4 {
            pool_manager,
            pool_id,
            ..
        } => (
            PoolKind::V4,
            FlightTarget::V4 {
                pool_manager: *pool_manager,
                pool_id: *pool_id,
            },
        ),
    };
    FlightKey {
        kind,
        chain_id,
        target,
    }
}

/// Publish `err` as a flight's failure: the exact value the leader raised.
pub(crate) fn failure(py: Python<'_>, err: &PyErr) -> PythonFlightFailure {
    Arc::new(err.value(py).clone().unbind())
}

/// Park GIL-free until the leader publishes, and answer its outcome.
pub(crate) fn wait(
    py: Python<'_>,
    peer: FlightSlotHandle<PythonFlightFailure>,
) -> PythonFlightOutcome {
    py.detach(move || degenbot_core::runtime::bridged_block_on(peer.wait()))
}

/// Re-raise a published failure as a `PyErr` under the caller's GIL token.
///
/// The value is cloned (not attached) here — the caller holds the GIL — so the
/// peer raises the leader's own exception instance, class and payload intact.
pub(crate) fn rethrow(py: Python<'_>, failure: &PythonFlightFailure) -> PyErr {
    PyErr::from_value(failure.as_ref().clone_ref(py).into_bound(py).into_any())
}

/// The claim source a build re-enters through: the driver's single-flight
/// table, or a scripted claimant that replays an election arc.
///
/// [`run`] is generic over this so its arc is drivable without manufacturing a
/// real election. Production has one implementation — the core table — and the
/// build path is unchanged by that generality: every `lead` runs under the
/// `FlightEnter::Lead` guard this source hands out.
pub(crate) trait FlightClaims {
    /// Lead the flight for `key`, or subscribe to the leader already on it.
    fn enter(&self, key: FlightKey) -> FlightEnter<'_, PythonFlightFailure>;
}

impl FlightClaims for PythonBuildFlights {
    fn enter(&self, key: FlightKey) -> FlightEnter<'_, PythonFlightFailure> {
        BuildFlights::enter(self, key)
    }
}

/// The bounded retry arc of a caller that keeps losing the claim election.
///
/// A `Built` wake re-answers through the registry pre-check: the leader built
/// and registered before publishing, so a claim the pre-check cannot confirm
/// means a concurrent caller won the registration — not that this caller
/// should build again. Each such wake is a lost claim; past [`Self::BOUND`] the
/// caller refuses, because a second build outside a claim is exactly what the
/// flight exists to prevent.
pub(crate) struct LostClaims(u32);

impl LostClaims {
    /// Lost claims a caller rides before answering exhaustion.
    pub(crate) const BOUND: u32 = 8;

    #[must_use]
    pub(crate) fn new() -> Self {
        Self(0)
    }

    /// Record one lost claim; `Err` is the terminal exhaustion answer.
    fn record(&mut self, key: FlightKey) -> PyResult<()> {
        self.0 += 1;
        if self.0 > Self::BOUND {
            return Err(PyRuntimeError::new_err(format!(
                "build-flight claim exhausted after {} lost claims (key {key:?}): the pool was \
                 built under another claim and the registry pre-check never confirmed it",
                self.0
            )));
        }
        Ok(())
    }
}

impl Default for LostClaims {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one pool build under the single flight: `precheck` answers from the
/// driver's registry of record, `lead` runs the full build choreography exactly
/// once under `key`'s claim, and peers park GIL-free until the leader publishes.
///
/// The only build is a leader's: a peer never runs `lead`, and a caller that
/// keeps losing the claim election refuses with [`LostClaims`]'s typed answer
/// rather than building outside a claim. A peer riding a leader's failure
/// re-raises the leader's exact exception value; a lead that ends without
/// publishing answers the table's lead-lost failure.
pub(crate) fn run<C, T>(
    py: Python<'_>,
    claims: &C,
    key: FlightKey,
    mut precheck: impl FnMut() -> PyResult<Option<T>>,
    lead: impl FnOnce() -> PyResult<(u64, T)>,
) -> PyResult<T>
where
    C: FlightClaims,
{
    let mut lost = LostClaims::new();
    loop {
        if let Some(payload) = precheck()? {
            return Ok(payload);
        }
        match claims.enter(key) {
            FlightEnter::Lead(mut guard) => {
                let outcome = lead();
                match &outcome {
                    Ok((pool_id, _)) => guard.publish(FlightOutcome::Built(*pool_id)),
                    Err(err) => guard.publish(FlightOutcome::Failed(failure(py, err))),
                }
                return outcome.map(|(_, payload)| payload);
            }
            FlightEnter::Peer(peer) => {
                // Park GIL-free; `Built` re-answers through `precheck`, a
                // failure re-raises the leader's exact exception.
                if let FlightOutcome::Failed(published) = wait(py, peer) {
                    return Err(rethrow(py, &published));
                }
                lost.record(key)?;
            }
        }
    }
}

/// The answer a peer receives when its leader ended without publishing: a
/// leader bug, raised as the driver's own `RuntimeError`.
fn lead_lost(key: FlightKey) -> PythonFlightFailure {
    Python::attach(|py| {
        let err = PyRuntimeError::new_err(format!(
            "build flight dropped unpublished (key {key:?}) — leader bug"
        ));
        Arc::new(err.value(py).clone().unbind())
    })
}

#[cfg(test)]
#[expect(clippy::panic, reason = "test assertions fail loudly")]
mod tests {
    use alloy::primitives::{address, Address};
    use degenbot_bot::bot_core::V4PoolBuildOverrides;
    use degenbot_bot::crawl::FlightEnter;
    use parking_lot::Mutex;
    use pyo3::exceptions::PyValueError;
    use std::collections::VecDeque;

    use super::*;

    const POOL: Address = address!("1111111111111111111111111111111111111111");
    const MANAGER: Address = address!("4444444444444444444444444444444444444444");

    fn address_key(addr: Address) -> FlightKey {
        FlightKey {
            kind: PoolKind::V2,
            chain_id: 1,
            target: FlightTarget::Address(addr),
        }
    }

    fn address_construction(addr: Address) -> PoolConstruction<'static> {
        PoolConstruction::V2 {
            address: addr,
            factory: None,
        }
    }

    fn v4_construction(pool_manager: Address, pool_id: [u8; 32]) -> PoolConstruction<'static> {
        PoolConstruction::V4 {
            pool_manager,
            pool_id,
            overrides: V4PoolBuildOverrides::default(),
            db: None,
        }
    }

    #[test]
    fn the_flight_key_is_the_constructions_registered_identity() {
        assert_eq!(
            flight_key(&address_construction(POOL), 1),
            address_key(POOL),
            "a V2 construction keys on its address"
        );

        let v3 = PoolConstruction::V3 {
            address: POOL,
            factory: None,
            db: None,
            fetcher: None,
            slot_layout: None,
        };
        assert_eq!(
            flight_key(&v3, 1),
            FlightKey {
                kind: PoolKind::V3,
                ..address_key(POOL)
            }
        );

        // Every address-keyed non-V3 family registers under the same address
        // identity, so they share the address-keyed flight class.
        for construction in [
            PoolConstruction::AerodromeV2 {
                address: POOL,
                factory: None,
            },
            PoolConstruction::Curve {
                address: POOL,
                registries: vec![],
            },
            PoolConstruction::BalancerWeighted {
                address: POOL,
                vault: MANAGER,
            },
            PoolConstruction::BalancerStable {
                address: POOL,
                vault: MANAGER,
                invariant_version: None,
            },
        ] {
            assert_eq!(flight_key(&construction, 1), address_key(POOL));
        }
    }

    #[test]
    fn the_v4_flight_identity_is_the_manager_and_pool_id_pair() {
        let first = [1u8; 32];
        let second = [2u8; 32];

        assert_eq!(
            flight_key(&v4_construction(MANAGER, first), 1),
            FlightKey {
                kind: PoolKind::V4,
                chain_id: 1,
                target: FlightTarget::V4 {
                    pool_manager: MANAGER,
                    pool_id: first,
                },
            }
        );

        // One manager hosts many pools: distinct ids are distinct flights, and
        // the pair never collides with an address-keyed flight on the manager.
        let one = address_key(MANAGER);
        let v4_one = flight_key(&v4_construction(MANAGER, first), 1);
        let v4_two = flight_key(&v4_construction(MANAGER, second), 1);
        assert_ne!(v4_one, v4_two);
        assert_ne!(v4_one, one);
    }

    #[test]
    fn one_key_is_one_flight() {
        Python::attach(|_py| {
            let table = table();
            let key = address_key(POOL);
            let FlightEnter::Lead(_lead) = table.enter(key) else {
                panic!("the first entrant leads");
            };
            assert!(
                matches!(table.enter(key), FlightEnter::Peer(_)),
                "a concurrent entrant for the same key parks"
            );
            // A different identity is its own flight (its guard drops at the
            // end of the assertion, publishing the lead-lost answer).
            assert!(matches!(
                table.enter(address_key(MANAGER)),
                FlightEnter::Lead(_)
            ));
        });
    }

    #[test]
    fn the_leaders_exact_exception_class_reaches_a_peer() {
        Python::attach(|py| {
            let table = table();
            let key = address_key(POOL);
            let FlightEnter::Lead(mut lead) = table.enter(key) else {
                panic!("the first entrant leads");
            };
            let FlightEnter::Peer(peer) = table.enter(key) else {
                panic!("a concurrent entrant parks");
            };

            lead.publish(FlightOutcome::Failed(failure(
                py,
                &PyValueError::new_err("boom"),
            )));

            let FlightOutcome::Failed(published) = wait(py, peer) else {
                panic!("the peer must ride the leader's failure");
            };
            let reraised = rethrow(py, &published);
            assert!(
                reraised.is_instance_of::<PyValueError>(py),
                "the peer raises the leader's exact exception class, got {reraised}"
            );
            assert_eq!(
                reraised.value(py).to_string(),
                "boom",
                "the leader's payload crosses intact"
            );
        });
    }

    #[test]
    fn a_cancelled_leader_answers_its_peers_with_a_runtime_error() {
        Python::attach(|py| {
            let table = table();
            let key = address_key(POOL);
            let FlightEnter::Lead(lead) = table.enter(key) else {
                panic!("the first entrant leads");
            };
            let FlightEnter::Peer(peer) = table.enter(key) else {
                panic!("a concurrent entrant parks");
            };

            // The lead ends without an answer — a panic or a cancelled future.
            drop(lead);

            let FlightOutcome::Failed(published) = wait(py, peer) else {
                panic!("a dead lead must publish the table's lead-lost answer");
            };
            let reraised = rethrow(py, &published);
            assert!(reraised.is_instance_of::<PyRuntimeError>(py));
            assert!(
                reraised.value(py).to_string().contains("leader bug"),
                "the abandoned lead is reported as the leader bug it is"
            );
        });
    }

    /// A claim source replaying a scripted election. Every subscription it
    /// hands out is a genuine slot minted by the driver's real table — a `Lead`
    /// plus a `Peer` handle, then a publication — so the arc a test observes is
    /// the protocol's, never a stub's invention.
    struct ScriptedClaims {
        peers: Mutex<VecDeque<FlightSlotHandle<PythonFlightFailure>>>,
        entered: Mutex<u32>,
    }

    impl ScriptedClaims {
        /// A script of `rounds` published claims for `key`.
        fn script(key: FlightKey, rounds: u32) -> Self {
            let scripted = table();
            Self {
                peers: Mutex::new((0..rounds).map(|_| built_peer(&scripted, key)).collect()),
                entered: Mutex::new(0),
            }
        }

        fn with_peers(peers: Vec<FlightSlotHandle<PythonFlightFailure>>) -> Self {
            Self {
                peers: Mutex::new(peers.into()),
                entered: Mutex::new(0),
            }
        }

        fn entered(&self) -> u32 {
            *self.entered.lock()
        }
    }

    impl FlightClaims for ScriptedClaims {
        fn enter(&self, _key: FlightKey) -> FlightEnter<'_, PythonFlightFailure> {
            *self.entered.lock() += 1;
            let Some(peer) = self.peers.lock().pop_front() else {
                panic!("the script ran out of claims");
            };
            FlightEnter::Peer(peer)
        }
    }

    /// One scripted round: a leader claims `key`, a peer subscribes, and the
    /// leader publishes the registered id — the table's publication contract.
    fn built_peer(
        flights: &PythonBuildFlights,
        key: FlightKey,
    ) -> FlightSlotHandle<PythonFlightFailure> {
        let FlightEnter::Lead(mut lead) = flights.enter(key) else {
            panic!("a scripted round starts on an unclaimed key");
        };
        let FlightEnter::Peer(peer) = flights.enter(key) else {
            panic!("a scripted round subscribes a peer");
        };
        lead.publish(FlightOutcome::Built(7));
        peer
    }

    /// The closed-claim arc: a caller subscribes to claims whose leader already
    /// published while the registry pre-check keeps answering nothing. The
    /// caller keeps losing the election, so it answers the typed exhaustion
    /// error and never builds outside a claim.
    #[test]
    fn a_closed_claim_subscription_ends_in_typed_claim_exhaustion() {
        Python::attach(|py| {
            let key = address_key(POOL);
            // More answered claims than the bound allows: the surplus is proof
            // that no fallback build was attempted once the arc was spent.
            let claims = ScriptedClaims::script(key, LostClaims::BOUND + 4);
            let mut built: u32 = 0;
            let err = match run(
                py,
                &claims,
                key,
                || Ok(None),
                || {
                    built += 1;
                    Ok((7, ()))
                },
            ) {
                Ok(()) => panic!("claim exhaustion is the caller's answer"),
                Err(err) => err,
            };
            assert!(
                err.is_instance_of::<PyRuntimeError>(py),
                "claim exhaustion is a typed RuntimeError, got {err}"
            );
            let rendered = err.value(py).to_string();
            assert!(rendered.contains("claim exhausted"), "{rendered}");
            assert_eq!(built, 0, "no claim, no build");
            assert_eq!(
                claims.entered(),
                LostClaims::BOUND + 1,
                "the refusal is the bound's own terminal answer"
            );
        });
    }

    /// The ordinary peer arc: a leader publishes, the woken caller re-answers
    /// through the pre-check, and the answer — not the wake — is the return.
    #[test]
    fn a_peer_wake_re_answers_through_the_precheck() {
        Python::attach(|py| {
            let key = address_key(POOL);
            let claims = ScriptedClaims::script(key, 1);
            let mut checks: u32 = 0;
            let answer = match run(
                py,
                &claims,
                key,
                || {
                    checks += 1;
                    Ok((checks > 1).then_some("registered"))
                },
                || panic!("a woken peer never runs the build"),
            ) {
                Ok(answer) => answer,
                Err(err) => panic!("the wake does not answer, the pre-check does: {err}"),
            };
            assert_eq!(answer, "registered");
            assert_eq!(checks, 2, "the wake itself answers nothing");
            assert_eq!(claims.entered(), 1, "the peer rode one claim");
        });
    }

    /// A leader's failure is the caller's own: the leader's exact exception
    /// class and payload cross back unchanged, and the flight retires.
    #[test]
    fn a_leaders_failure_is_the_callers_own_exception() {
        Python::attach(|py| {
            let key = address_key(POOL);
            let flights = table();
            let err = match run(
                py,
                &flights,
                key,
                || Ok(None::<()>),
                || Err(PyValueError::new_err("boom")),
            ) {
                Ok(()) => panic!("the leader's failure is the caller's answer"),
                Err(err) => err,
            };
            assert!(
                err.is_instance_of::<PyValueError>(py),
                "the caller raises the leader's exact class, got {err}"
            );
            assert_eq!(err.value(py).to_string(), "boom");
            assert!(
                matches!(flights.enter(key), FlightEnter::Lead(_)),
                "an answered flight retires, so the next caller leads a fresh claim"
            );
        });
    }

    /// A leader cancelled before its peer parked answers the table's lead-lost
    /// failure: a `RuntimeError` naming the leader bug, never a retry and never
    /// a build.
    #[test]
    fn a_cancelled_leader_answers_its_caller_with_the_lead_lost_error() {
        Python::attach(|py| {
            let key = address_key(POOL);
            let flights = table();
            let FlightEnter::Lead(lead) = flights.enter(key) else {
                panic!("the scripted lead goes first");
            };
            let FlightEnter::Peer(peer) = flights.enter(key) else {
                panic!("the scripted caller subscribes before the cancellation");
            };
            // The lead ends without an answer — a panic or a cancelled future.
            drop(lead);

            let claims = ScriptedClaims::with_peers(vec![peer]);
            let err = match run(
                py,
                &claims,
                key,
                || Ok(None::<()>),
                || panic!("a dead lead's caller must not build"),
            ) {
                Ok(()) => panic!("the lead-lost answer is the caller's answer"),
                Err(err) => err,
            };
            assert!(err.is_instance_of::<PyRuntimeError>(py));
            assert!(
                err.value(py).to_string().contains("leader bug"),
                "the abandoned lead is reported as the leader bug it is"
            );
            assert_eq!(
                claims.entered(),
                1,
                "a failed claim is answered, never retried"
            );
        });
    }
}
