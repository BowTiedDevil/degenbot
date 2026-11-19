//! The `PyO3` driver's side of the ONE build-flight protocol.
//!
//! The table itself is the core's (`degenbot_bot::crawl::BuildFlights`): the
//! key, the leader election, the `await`, and the abandoned-lead contract are
//! one implementation, shared with the async Rust crawl. This module carries
//! the two things only the `PyO3` driver can supply:
//!
//! - the failure payload: the leader's exact exception value behind an `Arc`,
//!   because an `Arc` clone needs no GIL while a `Py` clone does — a peer
//!   re-raises the value only once it holds the GIL again;
//! - the flight identity: a translated [`PoolConstruction`] already names the
//!   registered identity (its address, or a V4 `(PoolManager, pool_id)` pair),
//!   so the key is read off the construction the build consumes rather than
//!   re-derived from the request.
//!
//! Waiting parks GIL-free (`Python::detach`) on the core slot's async wait,
//! driven on the shared runtime: a parked peer holds no GIL, so the detached
//! leader (or any other thread) can build, register, and publish.

use std::sync::Arc;

use degenbot_bot::bot_core::PoolConstruction;
use degenbot_bot::crawl::{BuildFlights, FlightKey, FlightOutcome, FlightSlotHandle, FlightTarget};
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
    use pyo3::exceptions::PyValueError;

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
}
