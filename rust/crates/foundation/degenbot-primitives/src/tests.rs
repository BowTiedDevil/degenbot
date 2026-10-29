//! Seam tests: the public interning contract, exercised through the crate root.
//!
//! - `interner` tests pin the dense-id contract (ids 0..n, first-touch order,
//!   exact lookups, reverse map).
//! - `frozen` tests pin the freeze contract (same ids survive, lookup without
//!   hashing still answers exactly).
//! - `hasher` tests pin [`WordHasher`] against std's `Hash` contract: equal
//!   keys hash equal through any hasher, and a [`FastHashMap`] behaves as a
//!   map over byte-string keys.

use super::{FastHashMap, FrozenInterner, Interner, WordHasher};
use std::hash::{Hash, Hasher};

/// A fixed-size byte key shaped like an EVM address (20 bytes) — the primary
/// intended key species. Hash flows through `Hasher::write`, as it does for
/// `alloy::primitives::Address`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct AddressKey([u8; 20]);

impl AddressKey {
    fn new(seed: u8) -> Self {
        let mut bytes = [0u8; 20];
        bytes[0] = seed;
        bytes[19] = seed.wrapping_mul(7);
        Self(bytes)
    }
}

/// A 32-byte key shaped like a topic0 / pool hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
struct Hash32Key([u8; 32]);

impl Hash32Key {
    fn new(seed: u8) -> Self {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        bytes[31] = seed.wrapping_add(1);
        Self(bytes)
    }
}

// ---------------------------------------------------------------------------
// Interner: the build-phase dense-id contract
// ---------------------------------------------------------------------------

#[test]
fn interning_assigns_dense_ids_in_first_touch_order() {
    let mut interner = Interner::new();
    let a = interner.intern(AddressKey::new(1));
    let b = interner.intern(AddressKey::new(2));
    let c = interner.intern(AddressKey::new(3));

    assert_eq!((a, b, c), (0, 1, 2), "first touch assigns 0..n in order");
    assert_eq!(interner.len(), 3);
    assert!(!interner.is_empty());
}

#[test]
fn re_interning_returns_the_stable_first_touch_id() {
    let mut interner = Interner::new();
    let a = interner.intern(AddressKey::new(1));
    let b = interner.intern(AddressKey::new(2));

    assert_eq!(
        interner.intern(AddressKey::new(1)),
        a,
        "re-intern is stable"
    );
    assert_eq!(interner.intern(AddressKey::new(2)), b);
    assert_eq!(interner.len(), 2, "re-interning adds no key");
}

#[test]
fn interner_lookup_is_exact() {
    let mut interner = Interner::new();
    let a = interner.intern(AddressKey::new(1));

    assert_eq!(interner.get(&AddressKey::new(1)), Some(a));
    assert_eq!(interner.get(&AddressKey::new(9)), None);
    assert!(interner.contains(&AddressKey::new(1)));
    assert!(!interner.contains(&AddressKey::new(9)));
}

#[test]
fn interner_reverse_map_round_trips() {
    let mut interner = Interner::new();
    let a = interner.intern(AddressKey::new(1));
    let b = interner.intern(AddressKey::new(2));

    assert_eq!(interner.key(a), Some(&AddressKey::new(1)));
    assert_eq!(interner.key(b), Some(&AddressKey::new(2)));
    assert_eq!(interner.key(2), None, "no key past the dense range");
}

#[test]
fn interning_32_byte_keys_stays_dense() {
    let mut interner = Interner::new();
    let ids: Vec<u32> = (0..50u8)
        .map(|seed| interner.intern(Hash32Key::new(seed)))
        .collect();

    assert_eq!(ids, (0..50).collect::<Vec<_>>(), "ids are exactly 0..n");
    assert_eq!(interner.len(), 50);
}

#[test]
fn interner_with_capacity_starts_empty_and_dense() {
    let mut interner = Interner::with_capacity(64);
    assert!(interner.is_empty());

    let a = interner.intern(AddressKey::new(1));
    assert_eq!(a, 0);
}

// ---------------------------------------------------------------------------
// FrozenInterner: the freeze contract
// ---------------------------------------------------------------------------

#[test]
fn freeze_preserves_build_ids_and_answers_exactly() {
    // Non-sorted insertion order on purpose: freeze must keep BUILD ids, not
    // sorted positions.
    let mut interner = Interner::new();
    let seeds = [5u8, 1, 9, 3];
    let mut build_ids = Vec::new();
    for &seed in &seeds {
        build_ids.push((
            AddressKey::new(seed),
            interner.intern(AddressKey::new(seed)),
        ));
    }

    let frozen = interner.freeze();
    assert_eq!(frozen.len(), seeds.len());

    for (key, id) in &build_ids {
        assert_eq!(
            frozen.lookup(key),
            Some(*id),
            "frozen lookup must return the build-time id"
        );
    }
    assert_eq!(frozen.lookup(&AddressKey::new(42)), None);
    assert!(frozen.contains(&AddressKey::new(1)));
    assert!(!frozen.contains(&AddressKey::new(42)));
}

#[test]
fn frozen_interner_exposes_keys_by_build_id() {
    let mut interner = Interner::new();
    let a = interner.intern(AddressKey::new(5));
    let b = interner.intern(AddressKey::new(1));

    let frozen = interner.freeze();
    assert_eq!(frozen.key(a), Some(&AddressKey::new(5)));
    assert_eq!(frozen.key(b), Some(&AddressKey::new(1)));
    assert_eq!(frozen.key(2), None);
}

#[test]
fn frozen_iteration_is_in_first_touch_order() {
    let mut interner = Interner::new();
    for seed in [7u8, 2, 4] {
        interner.intern(AddressKey::new(seed));
    }

    let frozen = interner.freeze();
    let ids_and_seeds: Vec<(u32, u8)> = frozen.iter().map(|(id, key)| (id, key.0[0])).collect();
    assert_eq!(ids_and_seeds, vec![(0, 7), (1, 2), (2, 4)]);
}

#[test]
fn freezing_32_byte_keys_preserves_exact_lookup() {
    let mut interner = Interner::new();
    let mut expected = Vec::new();
    for seed in [200u8, 3, 128, 77] {
        expected.push((Hash32Key::new(seed), interner.intern(Hash32Key::new(seed))));
    }

    let frozen: FrozenInterner<Hash32Key> = interner.freeze();
    for (key, id) in expected {
        assert_eq!(frozen.lookup(&key), Some(id));
    }
    assert_eq!(frozen.lookup(&Hash32Key::new(1)), None);
}

#[test]
fn freezing_an_empty_interner_stays_empty() {
    let frozen: FrozenInterner<AddressKey> = Interner::new().freeze();
    assert!(frozen.is_empty());
    assert_eq!(frozen.len(), 0);
    assert_eq!(frozen.lookup(&AddressKey::new(1)), None);
}

// ---------------------------------------------------------------------------
// WordHasher / FastHashMap: the std Hash contract on byte keys
// ---------------------------------------------------------------------------

#[test]
fn word_hasher_is_deterministic_and_key_sensitive() {
    let hash_of = |bytes: &[u8]| {
        let mut hasher = WordHasher::default();
        hasher.write(bytes);
        hasher.finish()
    };

    assert_eq!(
        hash_of(b"fixed key"),
        hash_of(b"fixed key"),
        "equal bytes, equal hash"
    );
    assert_ne!(
        hash_of(b"fixed key"),
        hash_of(b"fixed Key"),
        "distinct bytes hash apart"
    );
    assert_ne!(
        hash_of(&[1u8; 20]),
        hash_of(&[1u8; 19]),
        "length participates in the hash"
    );
}

#[test]
fn word_hasher_agrees_with_std_hash_equivalence_for_address_keys() {
    // std's contract: equal keys must hash equal under ANY hasher. The map
    // seam only relies on that, never on hash values matching SipHash.
    let a = AddressKey::new(3);
    let a_copy = AddressKey::new(3);
    let b = AddressKey::new(4);

    let hash_with = |key: &AddressKey| {
        let mut hasher = WordHasher::default();
        key.hash(&mut hasher);
        hasher.finish()
    };

    assert_eq!(hash_with(&a), hash_with(&a_copy));
    assert_ne!(hash_with(&a), hash_with(&b));
}

#[test]
fn fast_hash_map_behaves_as_a_map_over_address_keys() {
    let mut map: FastHashMap<AddressKey, u64> = FastHashMap::default();
    for seed in 0..32u8 {
        map.insert(AddressKey::new(seed), u64::from(seed) * 100);
    }

    for seed in 0..32u8 {
        assert_eq!(
            map.get(&AddressKey::new(seed)),
            Some(&(u64::from(seed) * 100))
        );
    }
    assert_eq!(map.get(&AddressKey::new(200)), None);
    assert_eq!(map.len(), 32);
}

#[test]
fn fast_hash_map_over_32_byte_keys_inserts_and_probes() {
    let mut map: FastHashMap<Hash32Key, usize> = FastHashMap::default();
    for seed in 0..64u8 {
        map.insert(Hash32Key::new(seed), usize::from(seed));
    }
    assert_eq!(map.get(&Hash32Key::new(17)), Some(&17));
    assert_eq!(map.get(&Hash32Key::new(255)), None);
}

#[test]
fn word_hasher_write_u64_matches_write_of_le_bytes() {
    // The `write_u64` fast path must agree with the generic byte path on the
    // same value, or keys hashed through different Hash impls would disagree.
    let value = 0x0123_4567_89AB_CDEFu64;

    let mut fast = WordHasher::default();
    fast.write_u64(value);

    let mut generic = WordHasher::default();
    generic.write(&value.to_le_bytes());

    assert_eq!(fast.finish(), generic.finish());
}
