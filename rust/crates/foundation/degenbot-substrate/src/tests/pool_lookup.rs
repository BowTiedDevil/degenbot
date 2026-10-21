//! `PoolLookup` as a value: it resolves once, and it can key a memo.

use super::*;
use crate::{BotState, PoolLookup};

use alloy::primitives::Address;

/// Register a V4 pool (manager `[0x44; 20]`, on-chain id `[0xee; 32]`) so the
/// id round-trip covers the key-shaped family too.
fn register_v4(core: &mut BotState) -> u64 {
    core.register_v4_pool(&RegisterV4PoolParams {
        pool_manager: Address::from([0x44u8; 20]),
        pool_id: [0xeeu8; 32],
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    })
    .expect("test setup: V4 registration")
}

/// The id-keyed resolution the apply doors share: a registered V3 pool resolves
/// to its address key and that key resolves back to the id.
#[test]
fn from_pool_id_round_trips_a_v3_pool_to_its_address_key() {
    let mut state = BotState::new();
    let pool_id = register_v3(&mut state, 7);
    let key = PoolLookup::from_pool_id(&state, pool_id).expect("a registered V3 pool resolves");
    assert_eq!(key, PoolLookup::Address(make_pool_addr()));
    assert_eq!(
        key.resolve(&state),
        Some(pool_id),
        "the resolved key names the id it came from"
    );
}

/// The V4 twin: the id resolves to the `(pool_manager, pool_id)` key (the
/// shape the registry stores on the identity, not a derived stand-in) and back.
#[test]
fn from_pool_id_round_trips_a_v4_pool_to_its_key() {
    let mut state = BotState::new();
    let pool_id = register_v4(&mut state);
    let key = PoolLookup::from_pool_id(&state, pool_id).expect("a registered V4 pool resolves");
    assert_eq!(
        key,
        PoolLookup::V4 {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xeeu8; 32],
        }
    );
    assert_eq!(
        key.resolve(&state),
        Some(pool_id),
        "the resolved key names the id it came from"
    );
}

/// Only the two concentrated-liquidity families have a key: a V2 pool and an
/// unregistered id both have none.
#[test]
fn from_pool_id_refuses_a_non_concentrated_or_unregistered_id() {
    let mut state = BotState::new();
    let v2_id = state
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    assert_eq!(
        PoolLookup::from_pool_id(&state, v2_id),
        None,
        "a V2 pool has no concentrated-liquidity key"
    );
    assert_eq!(
        PoolLookup::from_pool_id(&state, 999_999),
        None,
        "an unregistered id has no key"
    );
}

#[test]
fn an_unregistered_lookup_resolves_to_none() {
    let state = BotState::new();
    assert_eq!(
        PoolLookup::from_address(Address::from([0x11u8; 20])).resolve(&state),
        None
    );
    assert_eq!(
        PoolLookup::V4 {
            pool_manager: Address::from([0x22u8; 20]),
            pool_id: [0x33u8; 32],
        }
        .resolve(&state),
        None
    );
}

/// The derives that let a lookup key a `Memo`: total order, equality, hashing.
#[test]
fn a_lookup_is_a_memo_key() {
    let lo = PoolLookup::from_address(Address::from([0x01u8; 20]));
    let hi = PoolLookup::from_address(Address::from([0x02u8; 20]));
    let v4 = PoolLookup::V4 {
        pool_manager: Address::from([0x01u8; 20]),
        pool_id: [0x00u8; 32],
    };
    assert!(lo < hi);
    assert_ne!(lo, v4);
    let mut keyed = std::collections::BTreeMap::new();
    keyed.insert(lo, "lo");
    keyed.insert(hi, "hi");
    keyed.insert(v4, "v4");
    assert_eq!(keyed.len(), 3);
    assert_eq!(keyed.get(&lo), Some(&"lo"));
    let mut hashed = std::collections::HashSet::new();
    assert!(hashed.insert(lo) && !hashed.insert(lo));
}
/// A family-mismatched state update is refused, not silently ignored.
#[test]
fn a_family_mismatch_is_refused() {
    let mut state = BotState::new();
    let refusal = state
        .sync_pool_state(
            PoolLookup::from_address(Address::from([0x11u8; 20])),
            degenbot_pools::state_update::PoolStateUpdate::V4(
                degenbot_pools::v4_state::V4StateSync {
                    sqrt_price_x96: alloy::primitives::U256::ZERO,
                    liquidity: 0,
                    tick: 0,
                    tick_data: hashbrown::HashMap::new(),
                    update_block: 0,
                },
            ),
        )
        .expect_err("a V4 payload on a V3 key must be refused");
    assert_eq!(
        refusal,
        degenbot_pools::state_update::PoolRefusal::FamilyMismatch {
            key: degenbot_pools::registry::RegisteredPoolFamily::V3,
            value: degenbot_pools::registry::RegisteredPoolFamily::V4,
        }
    );
}

/// A well-formed update for an unregistered pool is the one expected refusal.
#[test]
fn an_unregistered_pool_is_refused() {
    let mut state = BotState::new();
    let refusal = state
        .sync_pool_state(
            PoolLookup::from_address(Address::from([0x12u8; 20])),
            degenbot_pools::state_update::PoolStateUpdate::V3(
                degenbot_pools::v3_state::V3StateSync {
                    sqrt_price_x96: alloy::primitives::U256::ZERO,
                    liquidity: 0,
                    tick: 0,
                    tick_data: hashbrown::HashMap::new(),
                    update_block: 0,
                },
            ),
        )
        .expect_err("an unregistered pool must be refused");
    assert_eq!(
        refusal,
        degenbot_pools::state_update::PoolRefusal::Unregistered
    );
}
