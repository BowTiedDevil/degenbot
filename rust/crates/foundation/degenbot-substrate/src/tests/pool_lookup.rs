//! `PoolLookup` as a value: it resolves once, and it can key a memo.

use crate::{BotState, PoolLookup};

use alloy::primitives::Address;

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
