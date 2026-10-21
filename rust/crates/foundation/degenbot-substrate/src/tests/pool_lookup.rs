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
