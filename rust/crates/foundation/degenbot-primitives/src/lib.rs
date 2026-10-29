//! Dense-index primitives: intern fixed keys once, index arrays forever.
//!
//! Keys that are fixed for the life of a registry (pool addresses, token ids,
//! pool hashes) pay general-purpose hash-table costs on every lookup: hash the
//! key, pick a bucket, probe. When the key set is built once and then frozen,
//! the hot path needs none of that:
//!
//! 1. **Build phase** — [`Interner`] assigns each first-seen key a dense
//!    [`u32`] id, stable in first-touch order.
//! 2. **Freeze** — [`Interner::freeze`] hands back a [`FrozenInterner`]: a
//!    sorted lookup table (binary search, no hashing at lookup) over the same
//!    ids, plus the id-ordered key vector.
//! 3. **Hot phase** — side data lives in `Vec`s indexed by id; per-key lookup
//!    is one [`FrozenInterner::lookup`] call and all *by-id* access is plain
//!    array indexing. No hash table, no probe, no per-key allocation.
//!
//! For key sets that never freeze (mutable registries with removal), use
//! [`FastHashMap`] instead: a [`std::collections::HashMap`] keyed through
//! [`WordHasher`], which hashes fixed-size byte keys (addresses, hashes) in a
//! few multiplies instead of `SipHash`'s per-byte rounds.
//!
//! This crate is a zero-dependency leaf: `std` only, no alloy/pyo3/tokio.

use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher};

/// Multiplier for the `FxHash` scheme (rustc-hash's constants, inlined to keep
/// this leaf dependency-free). Shared with `degenbot-pathfinding`'s `U64Hasher`.
const FX_MULTIPLIER: u64 = 0x517C_C1B7_2722_0A95;

/// Hasher for fixed-size byte keys (addresses, 32-byte hashes, `u64` ids).
///
/// Folds whole words with the `FxHash` scheme so a 20-byte address is ~3
/// multiplies; the default `SipHash` walks the same bytes with several
/// instructions per byte and a finalization step, for keys that are internal
/// identifiers, not adversarial input.
#[derive(Default, Debug, Clone)]
pub struct WordHasher {
    hash: u64,
}

impl WordHasher {
    fn fold_word(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_MULTIPLIER);
    }

    fn fold_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.hash = (self.hash.rotate_left(5) ^ u64::from(b)).wrapping_mul(FX_MULTIPLIER);
        }
    }
}

impl Hasher for WordHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // `as_chunks` hands back exactly-8-byte arrays; no conversion to fail.
        let (words, remainder) = bytes.as_chunks::<8>();
        for word in words {
            self.fold_word(u64::from_le_bytes(*word));
        }
        self.fold_bytes(remainder);
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.fold_word(n);
    }
}

/// A [`HashMap`] keyed through [`WordHasher`].
///
/// The drop-in replacement for `HashMap<Address, V>`-shaped maps whose keys are
/// internal identifiers; drop-in for mutable key sets, no freeze required.
pub type FastHashMap<K, V> = HashMap<K, V, BuildHasherDefault<WordHasher>>;

/// Build-phase interner: assigns each first-seen key a dense [`u32`] id.
///
/// Ids are stable in first-touch order: re-interning a key returns the id it
/// already had, so callers can intern during registration while filling side
/// `Vec`s indexed by id.
#[derive(Debug, Clone)]
pub struct Interner<K, S: BuildHasher + Default = BuildHasherDefault<WordHasher>> {
    ids: HashMap<K, u32, S>,
    keys: Vec<K>,
}

impl<K> Interner<K, BuildHasherDefault<WordHasher>>
where
    K: Hash + Eq,
{
    /// A new empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// A new empty interner with room for `capacity` keys.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            ids: HashMap::with_capacity_and_hasher(capacity, BuildHasherDefault::default()),
            keys: Vec::with_capacity(capacity),
        }
    }
}

impl<K, S> Interner<K, S>
where
    K: Hash + Eq + Clone,
    S: BuildHasher + Default,
{
    /// Returns the id of `key`, assigning the next dense id on first sight.
    ///
    /// # Panics
    /// Panics if more than [`u32::MAX`] distinct keys are interned; a registry
    /// that large is a caller bug, not a runtime condition.
    // A registry exceeding u32::MAX distinct keys is a caller bug that must
    // fail loud, not silently corrupt ids; hence the reasoned expect.
    #[expect(clippy::expect_used)]
    pub fn intern(&mut self, key: K) -> u32 {
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = u32::try_from(self.keys.len()).expect("interner key census exceeded u32::MAX");
        self.keys.push(key.clone());
        self.ids.insert(key, id);
        id
    }

    /// The id assigned to `key`, or `None` if it was never interned.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<u32> {
        self.ids.get(key).copied()
    }

    /// Whether `key` has been interned.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.ids.contains_key(key)
    }

    /// The key assigned `id`, or `None` out of range.
    #[must_use]
    pub fn key(&self, id: u32) -> Option<&K> {
        self.keys.get(id as usize)
    }

    /// Number of interned keys (ids are exactly `0..len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether no keys have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Freezes the key set: no more interning, lookups stop hashing.
    ///
    /// The returned [`FrozenInterner`] hands out the *same* ids this interner
    /// assigned, so side tables built during the build phase stay valid.
    #[must_use]
    pub fn freeze(self) -> FrozenInterner<K>
    where
        K: Ord,
    {
        let mut index: Vec<(K, u32)> = self.ids.into_iter().collect();
        index.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        FrozenInterner {
            keys: self.keys,
            index,
        }
    }
}

impl<K> Default for Interner<K, BuildHasherDefault<WordHasher>>
where
    K: Hash + Eq,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Frozen key set: lookup by key without hashing, access by id by array.
///
/// Built by [`Interner::freeze`]; ids are exactly those the interner assigned.
#[derive(Debug, Clone)]
pub struct FrozenInterner<K> {
    keys: Vec<K>,
    index: Vec<(K, u32)>,
}

impl<K> FrozenInterner<K>
where
    K: Ord,
{
    /// The id assigned to `key` at build time, or `None` if absent.
    #[must_use]
    pub fn lookup(&self, key: &K) -> Option<u32> {
        self.index
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|pos| self.index[pos].1)
    }

    /// Whether `key` is in the frozen set.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.lookup(key).is_some()
    }

    /// The key assigned `id`, or `None` out of range.
    #[must_use]
    pub fn key(&self, id: u32) -> Option<&K> {
        self.keys.get(id as usize)
    }

    /// Number of keys (ids are exactly `0..len`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the frozen set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Iterates `(id, key)` in id (first-touch) order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &K)> {
        // `intern` bounds the census at u32::MAX, so every enumerate index
        // converts; `unwrap_or` only names the unreachable arm.
        self.keys
            .iter()
            .enumerate()
            .map(|(id, key)| (u32::try_from(id).unwrap_or(u32::MAX), key))
    }
}

#[cfg(test)]
mod tests;
