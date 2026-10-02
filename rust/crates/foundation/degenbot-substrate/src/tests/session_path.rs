//! Session path objects — the registry's half of the path kind.
//!
//! Seam: `crate::session_registry::{ObjectRefusal, PathHop,
//! PathIdentity, PathObject, PoolIdentity, PoolObject, SessionObjectRegistry}`
//! — the canonical path object (chain, ordered validated pool hops, and the
//! id the path-identity owner allocated) plus the hop signature the canonical
//! key rests on. The core half of this seam is testable without the engine: a
//! hop that names an unregistered pool, a request that arrives before the
//! owner is installed, and hop-signature equality all resolve inside the
//! registry.
//!
//! Terminology is the settled set in GLOSSARY.md § Session objects; the
//! identity-vs-policy split and the migration order are
//! `docs/architecture/session-object-registry.md`.

use super::*;

use std::sync::Arc;

use crate::session_registry::{
    ObjectRefusal, PathHop, PathIdentity, PathObject, PoolIdentity, PoolObject,
    SessionObjectRegistry,
};

/// Canonical identity is session-scoped, so the value only has to be a stable
/// per-test constant.
const CHAIN_ID: u64 = 1;

fn registry_with_pools(addresses: &[Address]) -> (SessionObjectRegistry, Vec<Arc<PoolObject>>) {
    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let pools = addresses
        .iter()
        .map(|address| registry.get_or_create_pool(PoolIdentity::v2(*address)))
        .collect();
    (registry, pools)
}

/// A path is built from VALIDATED pool references, so a hop naming a pool
/// this session never registered is refused rather than minted — and the
/// refusal names the pool, not the path, because the pool is the part that is
/// missing. Nothing is created: the hop is checked before the path-identity
/// owner is consulted, so a pool refusal cannot become a registration.
#[test]
fn a_path_over_an_unregistered_pool_is_a_typed_refusal_that_creates_nothing() {
    let (registry, _pools) = registry_with_pools(&[Address::from([0x11; 20])]);
    let unregistered = PoolIdentity::v2(Address::from([0x99; 20]));
    let registered = PoolIdentity::v2(Address::from([0x11; 20]));

    let refusal = registry
        .get_or_create_path(&[(unregistered.clone(), true), (registered, true)])
        .expect_err("a hop over a pool this session does not hold is refused");

    assert_eq!(
        refusal,
        ObjectRefusal::UnknownPoolIdentity {
            identity: unregistered
        }
    );
    // No pool was registered by the refused request, and no path exists.
    assert_eq!(registry.pool_count(), 1);
    assert_eq!(registry.path_count(), 0);
    // The refusal is the pool one, not the missing-owner one: hop validation
    // runs first, so an unwired session still answers the precise reason.
    assert!(!matches!(refusal, ObjectRefusal::NoPathOwner));
}

/// The registry holds no path identity of its own: without the path-identity
/// owner installed there is nothing to get-or-create and nothing to count, and
/// a path request answers with a typed refusal rather than a silently minted
/// object.
#[test]
fn a_path_request_before_the_owner_is_installed_is_a_typed_refusal() {
    let (registry, pools) = registry_with_pools(&[Address::from([0x11; 20])]);
    assert!(!registry.has_path_owner());
    assert_eq!(registry.path_count(), 0);

    let hops = [
        (PoolIdentity::v2(Address::from([0x11; 20])), true),
        (PoolIdentity::v2(Address::from([0x11; 20])), false),
    ];
    assert_eq!(
        registry.get_or_create_path(&hops).expect_err("no owner"),
        ObjectRefusal::NoPathOwner
    );
    assert_eq!(
        registry.resolve_path(&hops).expect_err("no owner"),
        ObjectRefusal::NoPathOwner
    );
    assert_eq!(registry.path_count(), 0);
    // The pool half of the request still resolved — the refusal is about the
    // path owner, not about the pools.
    assert!(Arc::ptr_eq(
        &pools[0],
        &registry
            .resolve_pool(&PoolIdentity::v2(Address::from([0x11; 20])))
            .expect("pool")
    ));
}

/// The canonical key is the ORDERED hop signature: hop order and per-hop
/// direction are both part of the identity, and a hop is named by the pool
/// object it validated to — so two requests naming the same pools the same way
/// are one path and anything else is another.
#[test]
fn the_canonical_key_is_the_ordered_hop_signature() {
    let (_registry, pools) =
        registry_with_pools(&[Address::from([0x11; 20]), Address::from([0x22; 20])]);
    let (a, b) = (Arc::clone(&pools[0]), Arc::clone(&pools[1]));

    let forward = PathIdentity::new(vec![
        PathHop::new(Arc::clone(&a), true),
        PathHop::new(Arc::clone(&b), false),
    ]);
    let same = PathIdentity::new(vec![
        PathHop::new(Arc::clone(&a), true),
        PathHop::new(Arc::clone(&b), false),
    ]);
    let flipped = PathIdentity::new(vec![
        PathHop::new(Arc::clone(&a), false),
        PathHop::new(Arc::clone(&b), true),
    ]);
    let reordered = PathIdentity::new(vec![
        PathHop::new(Arc::clone(&b), false),
        PathHop::new(Arc::clone(&a), true),
    ]);

    assert_eq!(forward, same);
    assert_ne!(forward, flipped);
    assert_ne!(forward, reordered);
    assert!(forward.hops()[0].zero_for_one());
    assert!(!forward.hops()[1].zero_for_one());
    assert!(Arc::ptr_eq(forward.hops()[0].pool(), &a));

    // The hops are the session's canonical pool objects, so the path identity
    // joins live state through them rather than through a second id map.
    let other_session = SessionObjectRegistry::new(CHAIN_ID);
    let twin = other_session.get_or_create_pool(PoolIdentity::v2(Address::from([0x11; 20])));
    assert!(
        !Arc::ptr_eq(&twin, &a),
        "canonical identity is session-local"
    );
}

/// The canonical object is identity: chain, hop signature, and the id the
/// path-identity owner allocated. A strategy's plan is derived FROM it and
/// never stored in it, so it reads back the same whatever the reader intends
/// to do with it.
#[test]
fn the_canonical_object_is_identity_only() {
    let (_registry, pools) = registry_with_pools(&[Address::from([0x11; 20])]);
    let identity = PathIdentity::new(vec![PathHop::new(Arc::clone(&pools[0]), true)]);
    let object = PathObject::mint(CHAIN_ID, identity.clone(), 7);

    assert_eq!(object.chain_id(), CHAIN_ID);
    assert_eq!(object.path_id(), 7);
    assert_eq!(object.identity(), &identity);
    assert_eq!(object.hops().len(), 1);
    assert_eq!(object, object.clone());
}
