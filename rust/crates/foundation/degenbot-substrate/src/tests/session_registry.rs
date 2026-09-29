//! Session object registry: canonical identity and get-or-create.
//!
//! Seam: `crate::session_registry::{ObjectRefusal, PoolIdentity,
//! PoolObject, SessionObjectRegistry, TokenIdentity, TokenObject}` — the
//! session object registry, the single per-session owner of object identity
//! (identity only: no chain or database I/O, no solver behavior, no
//! submission policy). `BotState` remains the live-state owner, so these
//! tests read it through the in-memory registration fixtures in this module
//! and never write live state through the registry.
//!
//! The registry's get-or-create and resolve surfaces hand back owned shared
//! handles (`Arc<PoolObject>` / `Arc<TokenObject>`), never `&PoolObject`.
//! An identity-only registry over the workspace's `dashmap` + `parking_lot`
//! dependencies has no stable-address storage, so a `&self` method cannot
//! return a borrow tied to an interior-mutable entry; `Arc` is also the
//! honest shape, because a consumer holds the object beyond the call and the
//! registry — not the consumer — owns the session's entry. Canonical
//! identity is therefore observed with `Arc::ptr_eq`, which compares the
//! allocation, not a field.
//!
//! Terminology is the settled set in CONTEXT.md § Session objects (Object,
//! Session object registry, Canonical identity, Get-or-create, Live state,
//! Object reference); the rationale is
//! docs/architecture/session-object-registry.md.
//!
//! The module compiles only against a registry that provides the seam above.
//! Each name in that list is a contract requirement, so renaming one here is
//! a contract change rather than a mechanical edit.

use super::*;

use std::sync::{Arc, Barrier, Mutex};

use crate::session_registry::{
    ObjectRefusal, PoolIdentity, PoolObject, SessionObjectRegistry, TokenIdentity, TokenObject,
};

use degenbot_decoders::v4_swap_decoder::V4PoolId;

/// The chain these fixtures are shaped for. Canonical identity is
/// session-scoped, so a registry's chain is a constructor argument and the
/// value only has to be a stable per-test constant.
const CHAIN_ID: u64 = 1;

/// V4 registration params for one `(pool_manager, pool_id)` pair — a live
/// fixture, so the identity assertions below sit beside real live state.
pub(super) fn v4_params(pool_manager: Address, pool_id: V4PoolId) -> RegisterV4PoolParams {
    RegisterV4PoolParams {
        pool_manager,
        pool_id,
        pool_key: V4PoolKey {
            currency0: make_token0(),
            currency1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    }
}

/// One canonical object per chain-scoped identity, and the registry is not a
/// second live-state writer: repeated requests for the same identity hand
/// back the first object while `BotState` keeps the single live entry and
/// refuses its own duplicate.
#[test]
fn one_canonical_object_per_chain_scoped_identity() {
    let mut state = BotState::new();
    let params = make_params(U112::from(1000), U112::from(2000));
    state
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let first: Arc<PoolObject> = registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr()));
    let second: Arc<PoolObject> = registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr()));

    assert!(
        Arc::ptr_eq(&first, &second),
        "a second request for one identity returns the first object, not a twin"
    );
    assert_eq!(registry.pool_count(), 1, "one identity, one registry entry");

    assert!(
        matches!(
            state.register_v2_pool(&params),
            Err(RegisterV2PoolError::AlreadyRegistered { .. })
        ),
        "live state stays with BotState, which refuses a duplicate address"
    );
    assert_eq!(
        state.pool_count(),
        1,
        "the registry added no live-state entry"
    );
    assert_eq!(
        registry.pool_count(),
        1,
        "a live-state refusal did not create a second object"
    );
}

/// Canonical identity is session-local and chain-scoped: the same address in
/// another chain, and in another session on the same chain, is a different
/// object. Nothing about identity is process-global.
#[test]
fn canonical_identity_does_not_cross_chains_or_sessions() {
    let this_session = SessionObjectRegistry::new(CHAIN_ID);
    let other_chain = SessionObjectRegistry::new(42_153);
    let other_session = SessionObjectRegistry::new(CHAIN_ID);

    let here = this_session.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));
    let there = other_chain.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));
    let elsewhere = other_session.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));

    assert!(
        !Arc::ptr_eq(&here, &there),
        "the same pool address on another chain is another canonical identity"
    );
    assert!(
        !Arc::ptr_eq(&here, &elsewhere),
        "canonical identity is per session, not a process-wide key space"
    );
    assert_eq!(this_session.pool_count(), 1);
    assert_eq!(other_chain.pool_count(), 1);
    assert_eq!(other_session.pool_count(), 1);
}

/// A V2 and a V3 pool at the same address are two canonical identities
/// (family is part of a pool's key), and each family is live in `BotState`
/// under its own entry.
#[test]
fn v2_and_v3_pool_identity_is_family_scoped_address() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);

    let as_v2 = registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr()));
    let as_v3 = registry.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));

    assert!(
        !Arc::ptr_eq(&as_v2, &as_v3),
        "family is part of a V2/V3 pool's canonical identity"
    );
    assert!(
        Arc::ptr_eq(
            &as_v2,
            &registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr()))
        ),
        "the V2 object is canonical for its identity"
    );
    assert!(
        Arc::ptr_eq(
            &as_v3,
            &registry.get_or_create_pool(PoolIdentity::v3(make_pool_addr()))
        ),
        "the V3 object is canonical for its identity"
    );
    assert_eq!(
        registry.pool_count(),
        2,
        "one entry per identity, not per address"
    );

    let mut state = BotState::new();
    state
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    register_v3_on_core(&mut state, Address::from([0x11u8; 20]), 0);
    assert_eq!(
        state.pool_count(),
        2,
        "each family is its own live-state entry"
    );
}

/// A V4 pool's canonical identity is the `(pool_manager, pool_id)` pair —
/// `PoolManager` is the Uniswap V4 contract role and a key component, never a
/// stand-in for the registry. The same `pool_id` bytes under another
/// `PoolManager` is another object.
#[test]
fn v4_pool_identity_is_pool_manager_plus_pool_id() {
    let pool_manager = Address::from([0x44u8; 20]);
    let other_manager = Address::from([0x55u8; 20]);
    let pool_id: V4PoolId = [0xeeu8; 32];

    let mut state = BotState::new();
    state
        .register_v4_pool(&v4_params(pool_manager, pool_id))
        .expect("test setup: V4 registration");

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let canonical = registry.get_or_create_pool(PoolIdentity::v4(pool_manager, pool_id));
    assert!(
        Arc::ptr_eq(
            &canonical,
            &registry.get_or_create_pool(PoolIdentity::v4(pool_manager, pool_id)),
        ),
        "the same (pool_manager, pool_id) is one canonical object"
    );
    assert!(
        !Arc::ptr_eq(
            &canonical,
            &registry.get_or_create_pool(PoolIdentity::v4(other_manager, pool_id)),
        ),
        "the same pool_id under another PoolManager is another pool"
    );
    assert_eq!(registry.pool_count(), 2);

    assert!(
        matches!(
            state.register_v4_pool(&v4_params(pool_manager, pool_id)),
            Err(RegisterV4PoolError::AlreadyRegistered { .. })
        ),
        "live state keys V4 by the same pair and holds one entry"
    );
    assert_eq!(state.pool_count(), 1);
}

/// An ERC-20 token's canonical identity is its chain-scoped address: the same
/// address is one object, a different address is another, and `BotState`
/// keeps the live token entry.
#[test]
fn erc20_token_identity_is_the_chain_scoped_address() {
    let mut state = BotState::new();
    state.register_token(
        make_token0(),
        "WETH".to_owned(),
        "WETH".to_owned(),
        18,
        CHAIN_ID,
    );

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let first: Arc<TokenObject> = registry.get_or_create_token(TokenIdentity::erc20(make_token0()));
    let again: Arc<TokenObject> = registry.get_or_create_token(TokenIdentity::erc20(make_token0()));
    let other: Arc<TokenObject> = registry.get_or_create_token(TokenIdentity::erc20(make_token1()));

    assert!(
        Arc::ptr_eq(&first, &again),
        "one token address is one canonical object"
    );
    assert!(
        !Arc::ptr_eq(&first, &other),
        "a different token address is a different object"
    );
    assert_eq!(registry.token_count(), 2);

    state.register_token(
        make_token0(),
        "WETH".to_owned(),
        "WETH".to_owned(),
        18,
        CHAIN_ID,
    );
    assert!(
        state.token_entry(&make_token0()).is_some(),
        "BotState keeps the live token entry"
    );
    assert_eq!(
        registry.token_count(),
        2,
        "an idempotent live-state registration created no second object"
    );
}

/// Get-or-create is the only registration semantic: any number of requests
/// for one identity, of either kind, converges on the same canonical object
/// and the same entry count.
#[test]
fn duplicate_get_or_create_returns_the_same_canonical_object() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);

    let mut pools: Vec<Arc<PoolObject>> = Vec::new();
    let mut tokens: Vec<Arc<TokenObject>> = Vec::new();
    for _ in 0..8 {
        pools.push(registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr())));
        tokens.push(registry.get_or_create_token(TokenIdentity::erc20(make_token0())));
    }

    let first_pool = &pools[0];
    assert!(
        pools.iter().all(|pool| Arc::ptr_eq(first_pool, pool)),
        "repeated get-or-create never returns a second pool object"
    );
    let first_token = &tokens[0];
    assert!(
        tokens.iter().all(|token| Arc::ptr_eq(first_token, token)),
        "repeated get-or-create never returns a second token object"
    );
    assert_eq!(registry.pool_count(), 1);
    assert_eq!(registry.token_count(), 1);
}

/// Get-or-create takes `&self` and a concurrent second request joins the
/// first: racing threads that all ask for one identity observe exactly one
/// canonical object and one registry entry. A caller-side lock around the
/// registry would make this assertion vacuous, so the race is driven through
/// the seam itself.
#[test]
fn concurrent_get_or_create_never_produces_two_canonical_objects() {
    const THREADS: usize = 8;

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let gate = Barrier::new(THREADS);
    let seen: Mutex<Vec<Arc<PoolObject>>> = Mutex::new(Vec::with_capacity(THREADS));

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                gate.wait();
                let object = registry.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));
                seen.lock()
                    .expect("test harness: the seen-handle list is not poisoned")
                    .push(object);
            });
        }
    });

    let handles = seen
        .into_inner()
        .expect("test harness: the seen-handle list is not poisoned");
    assert_eq!(handles.len(), THREADS);
    let first = &handles[0];
    assert!(
        handles.iter().all(|handle| Arc::ptr_eq(first, handle)),
        "racing get-or-create calls observed more than one canonical object"
    );
    assert_eq!(
        registry.pool_count(),
        1,
        "the race left exactly one registry entry"
    );
}

/// A resolve request for an identity the session does not hold is a typed
/// refusal that creates nothing: it is not a second object, and the refusal
/// does not pre-empt the one get-or-create that legitimately registers it.
#[test]
fn missing_identity_is_a_typed_refusal_not_a_silent_second_object() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);

    let pool = registry.resolve_pool(&PoolIdentity::v2(make_pool_addr()));
    assert!(
        matches!(pool, Err(ObjectRefusal::UnknownPoolIdentity { .. })),
        "an unregistered pool identity is refused by variant, not by a string"
    );
    assert_eq!(registry.pool_count(), 0, "a refusal registered nothing");

    let token = registry.resolve_token(&TokenIdentity::erc20(make_token0()));
    assert!(
        matches!(token, Err(ObjectRefusal::UnknownTokenIdentity { .. })),
        "an unregistered token identity is refused by variant, not by a string"
    );
    assert_eq!(registry.token_count(), 0, "a refusal registered nothing");

    let created = registry.get_or_create_pool(PoolIdentity::v2(make_pool_addr()));
    let resolved = registry
        .resolve_pool(&PoolIdentity::v2(make_pool_addr()))
        .expect("the identity is registered now");
    assert!(
        Arc::ptr_eq(&created, &resolved),
        "get-or-create after a refusal registers the one object, and resolve joins it"
    );
    assert_eq!(registry.pool_count(), 1);
}

/// Every family a registered pool can be (`BotState`'s `pool_family` tag
/// vocabulary) is a first-class canonical identity, so a consumer naming a
/// Balancer/Aerodrome/Curve pool gets the session's one object for it instead
/// of keeping a private per-family map. Family is still part of the key: the
/// same address registered under two families is two identities.
#[test]
fn every_registered_family_is_a_canonical_identity() {
    const FAMILIES: [&str; 6] = [
        "v2",
        "v3",
        "curve",
        "balancer-weighted",
        "balancer-stable",
        "aerodrome-v2",
    ];

    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let mut objects = Vec::with_capacity(FAMILIES.len());
    for family in FAMILIES {
        let identity = PoolIdentity::for_address_family(family, make_pool_addr())
            .unwrap_or_else(|| panic!("{family} is a registered family with a canonical identity"));
        assert_eq!(
            identity.family_tag(),
            family,
            "the identity reports the same family tag the name came in as"
        );
        objects.push(registry.get_or_create_pool(identity));
    }

    let first = &objects[0];
    assert!(
        objects[1..]
            .iter()
            .all(|object| !Arc::ptr_eq(first, object)),
        "one address under two families is two canonical identities, not one"
    );
    assert_eq!(registry.pool_count(), FAMILIES.len());

    // A V4 pool's identity is the `(PoolManager, pool_id)` pair, never an
    // address, so the address-keyed constructor must refuse the tag instead of
    // inventing a V4 identity whose key is only half of it.
    assert!(
        PoolIdentity::for_address_family("v4", make_pool_addr()).is_none(),
        "the V4 family is not address-keyed; it needs a (PoolManager, pool_id) pair"
    );
    assert!(
        PoolIdentity::for_address_family("not-a-family", make_pool_addr()).is_none(),
        "an unknown family tag names no identity"
    );
}

/// A consumer that knows only an address — the shape every address-keyed pool
/// lookup in the Python facade has — resolves the session's canonical object
/// through the registry's address index instead of consulting a second map.
/// The index is an index: it names the object the identity map already holds,
/// and an unregistered address is a typed refusal, not a miss that invents one.
#[test]
fn address_index_resolves_the_object_the_identity_map_holds() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);
    assert!(
        matches!(
            registry.resolve_pool_by_address(&make_pool_addr()),
            Err(ObjectRefusal::UnknownPoolAddress { .. })
        ),
        "an address this session does not hold is refused by variant"
    );

    let canonical = registry.get_or_create_pool(PoolIdentity::v3(make_pool_addr()));
    let resolved = registry
        .resolve_pool_by_address(&make_pool_addr())
        .expect("the address is registered now");
    assert!(
        Arc::ptr_eq(&canonical, &resolved),
        "the address index joins the one canonical object, never a twin"
    );

    // A second family at the same address is a second identity, and the
    // address view names the first one registered: the index reports which
    // object an address-only consumer resolves to rather than picking per call.
    let balancer = registry.get_or_create_pool(
        PoolIdentity::for_address_family("balancer-weighted", make_pool_addr())
            .expect("test setup: balancer-weighted is a canonical family"),
    );
    assert_eq!(registry.pool_count(), 2);
    assert!(
        Arc::ptr_eq(
            &canonical,
            &registry
                .resolve_pool_by_address(&make_pool_addr())
                .expect("the address is registered")
        ),
        "the address view stays on the first identity registered for the address"
    );
    assert!(
        !Arc::ptr_eq(&balancer, &resolved),
        "the second family is a distinct object even at the same address"
    );
}

/// A V4 pool is named by its `(PoolManager, pool_id)` pair, so it is
/// deliberately absent from the address index: a `PoolManager` contract address
/// is not a pool address, and indexing it there would let an address-only
/// consumer resolve a manager to one of the many pools it hosts.
#[test]
fn v4_pools_are_not_address_indexed() {
    let pool_manager = Address::from([0x44u8; 20]);
    let registry = SessionObjectRegistry::new(CHAIN_ID);
    let _ = registry.get_or_create_pool(PoolIdentity::v4(pool_manager, [0xeeu8; 32]));

    assert!(
        matches!(
            registry.resolve_pool_by_address(&pool_manager),
            Err(ObjectRefusal::UnknownPoolAddress { .. })
        ),
        "a PoolManager address names no address-keyed pool"
    );
    assert_eq!(registry.pool_count(), 1);
}

/// An Object reference is shared: two consumers hold the same canonical
/// object at once, and neither one's handle going away removes it. The
/// registry owns the entry for the session's lifetime.
#[test]
fn two_consumers_share_one_object_and_neither_owns_removal() {
    let registry = SessionObjectRegistry::new(CHAIN_ID);

    // kept alive across the consumers' scope, so the identity check below
    // compares against an entry that was never removed and re-registered.
    let witness: Arc<TokenObject> =
        registry.get_or_create_token(TokenIdentity::erc20(make_token0()));

    {
        let engine = registry.get_or_create_token(TokenIdentity::erc20(make_token0()));
        let strategy = registry.get_or_create_token(TokenIdentity::erc20(make_token0()));
        assert!(
            Arc::ptr_eq(&engine, &strategy),
            "two consumers share one canonical object, not one object each"
        );
    }

    assert_eq!(
        registry.token_count(),
        1,
        "dropping a consumer's object handle did not remove the entry"
    );
    let later: Arc<TokenObject> = registry.get_or_create_token(TokenIdentity::erc20(make_token0()));
    assert!(
        Arc::ptr_eq(&later, &witness),
        "a request after both consumers released still joins the first object"
    );
}
