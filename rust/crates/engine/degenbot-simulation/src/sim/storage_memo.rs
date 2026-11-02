//! `StorageMemo` - a per-anchor read memo in front of the RPC fallback.

//! SIMPIPE2 M2 (2026-09-05): the CacheDb/WarmCodeCache stack caches basic +
//! code, but `storage_ref` always forwards to RPC BY DESIGN - the engine
//! deliberately does not serve engine-carried slots (a partial serve
//! reintroduces the K-invariant / `LOK` reverts; see `bot_state_db.rs`).
//! The result: every payload sim pays ~20 cold storage RPC round-trips, and
//! the FFI-era per-fanout warmed handle never made it to the inline arms,
//! so ~N paths on the same pools refetch the same slots N times per cycle.

//! This memo is NOT the (refuted, gated) engine-serve seam: it caches the
//! SAME RPC pre-state for the lifetime of ONE sim block. All sims of a block
//! run at the same height on the same pre-state, and `execute()` writes are
//! absorbed by the sim-local `CacheDB` overlay - the memo only ever stores
//! values the fallback itself fetched. Semantically identical, strictly
//! fewer RPC trips. The owner recreates the memo on every block advance.
//!
//! # The scope: one anchor, shared by the anchor's fan-outs
//!
//! A memo entry is an RPC answer tagged with ONE read anchor - the block the
//! fallback pins its reads to ([`AlloyAsyncDb`](crate::sim::evm2::alloy_async_db)
//! passes it as `eth_getStorageAt`'s block tag, never `latest`). Two fan-outs
//! that pin the same anchor read the same chain state, so they can share one
//! memo; a fan-out at a different anchor cannot. [`StorageMemoCache`] is that
//! owner: it hands every fan-out at an anchor the SAME memo and retires it -
//! with every value it cached - the moment the anchor moves. Retirement is
//! unconditional, including a BACKWARD move: a reorg rewind lowers the anchor
//! and must never be served a memo filled at the height it rewound from.
//!
//! # The reorg fence: an anchor is a block, not a height
//!
//! A height alone cannot see a same-height reorg. When block N is replaced by a
//! different block N, the number a number-keyed memo is pinned to does not move
//! while the node's answers underneath it change, so the memo would keep
//! serving the abandoned block's words to fan-outs whose own fresh reads come
//! from the new canonical N. The key is therefore [`ReadAnchor`] - the number
//! AND the block's canonical hash - and a replacement block mints a fresh memo
//! exactly as a moved height does.

use alloy::primitives::{Address, StorageValue, B256, U256};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct Inner {
    map: HashMap<(Address, U256), StorageValue>,
}

#[derive(Default)]
pub struct StorageMemo {
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl StorageMemo {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Memo hit at this anchor (the memo is anchor-scoped: the owner mints a
    /// fresh one when the read anchor moves - see [`StorageMemoCache`]).
    pub fn get(&self, address: Address, index: U256) -> Option<StorageValue> {
        let inner = self.inner.lock();
        let hit = inner.map.get(&(address, index)).copied();
        if hit.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    pub fn put(&self, address: Address, index: U256, value: StorageValue) {
        self.inner.lock().map.insert((address, index), value);
    }

    /// Seed an entry with a value the caller read AT this memo's anchor — the
    /// prefetch lane's intake (see [`crate::sim::prefetch`]).
    ///
    /// The anchoring contract is the caller's to keep: a seeded entry is
    /// indistinguishable from one the fallback learned here, so it must be the
    /// node's answer at the anchor this memo is installed for. A value read at
    /// any other height belongs in that height's memo (the owner mints one per
    /// anchor), never in this one.
    pub fn seed(&self, address: Address, index: U256, value: StorageValue) {
        self.put(address, index, value);
    }

    /// Whether this memo already holds `(address, index)` — the prefetch's
    /// skip-if-known gate. A pure predicate: it moves neither counter, so a
    /// prefetch that defers to a warm memo leaves the hit/miss profile exactly
    /// as the run itself shaped it.
    #[must_use]
    pub fn holds(&self, address: Address, index: U256) -> bool {
        self.inner.lock().map.contains_key(&(address, index))
    }

    /// `(hits, misses)` - the lab counters for the probe logs.
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

/// The block a memo's values were read at - the key [`StorageMemoCache`] shares
/// and retires on.
///
/// `number` is the block the RPC fallback tags every read with; `hash` is that
/// block's canonical identity, the reorg fence. Both must match for a fan-out
/// to inherit an installed memo: a same-height reorg leaves the number alone
/// and changes only the hash, so only a hashed key can see it.
///
/// `hash: None` is the degrade: the caller could not resolve the block's
/// identity (no ambient runtime, an RPC failure, a caller that never asks), so
/// the memo retires on the number alone - the pre-fence scope, never a wrong
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadAnchor {
    /// The block number every read held by the memo is pinned to.
    pub number: u64,
    /// The canonical hash of that block, when the caller resolved it.
    pub hash: Option<B256>,
}

impl ReadAnchor {
    /// A number-only anchor: the memo retires when the height moves.
    #[must_use]
    pub const fn at(number: u64) -> Self {
        Self { number, hash: None }
    }

    /// The anchor of `number`, identified by its canonical `hash`.
    #[must_use]
    pub const fn identified(number: u64, hash: B256) -> Self {
        Self {
            number,
            hash: Some(hash),
        }
    }
}

/// The session-scoped owner of the per-anchor [`StorageMemo`].
///
/// One instance lives for the engine session's life (it rides the engine-owned
/// shared read caches, see `WarmCodeCacheInner`) and holds the memo of the
/// CURRENT read anchor. [`Self::for_anchor`] is the only way in: same anchor,
/// same memo (that is the cross-fan-out sharing); any other anchor, a brand new
/// memo - the retired one keeps serving only the fan-outs that already hold it,
/// which are still pinned at ITS anchor.
///
/// The anchor is the block the RPC fallback pins its reads to, so it must be
/// the handle's `sim_block`, never the driver's clock: the two differ whenever
/// the batch's promoted solve block lags the pump.
///
/// Two anti-parallel fan-outs at DIFFERENT anchors (one lagging batch while the
/// pump has already advanced) mint a memo each and share neither - correct, and
/// exactly the per-fan-out cost this scope started from. Sharing is what the
/// same-anchor case buys, and an anchor is a block, so the alternating case is
/// the lag window, not the steady state.
#[derive(Default)]
pub struct StorageMemoCache {
    current: Mutex<Option<(ReadAnchor, Arc<StorageMemo>)>>,
}

impl StorageMemoCache {
    /// An empty cache: no anchor installed yet (the first `for_anchor` mints
    /// the memo).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The memo for `anchor`: the installed one when the anchor is unchanged,
    /// otherwise a fresh empty memo that becomes the installed one.
    ///
    /// "Unchanged" is the whole [`ReadAnchor`] - number AND hash - so a
    /// same-height reorg mints a fresh memo exactly as a moved height does.
    /// The replacement drops every value cached at the previous anchor - the
    /// invalidation contract in one line (see the module docs).
    pub fn for_anchor(&self, anchor: ReadAnchor) -> Arc<StorageMemo> {
        let mut current = self.current.lock();
        if let Some((installed, memo)) = current.as_ref() {
            if *installed == anchor {
                return Arc::clone(memo);
            }
        }
        let memo = Arc::new(StorageMemo::new());
        *current = Some((anchor, Arc::clone(&memo)));
        memo
    }

    /// The installed anchor (`None` before the first `for_anchor`) - the
    /// invalidation seam's observable state.
    #[must_use]
    pub fn anchor(&self) -> Option<ReadAnchor> {
        self.current.lock().as_ref().map(|(anchor, _)| *anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);

    #[test]
    fn round_trip_and_stats() {
        let memo = StorageMemo::new();
        let addr = Address::ZERO;
        let key = U256::from(7u64);
        assert_eq!(memo.get(addr, key), None);
        memo.put(addr, key, StorageValue::from(42u64));
        assert_eq!(memo.get(addr, key), Some(StorageValue::from(42u64)));
        let (h, m) = memo.stats();
        assert_eq!((h, m), (1, 1));
    }

    /// The sharing contract: every fan-out at one anchor gets the SAME memo,
    /// so the anchor's cold chain is paid once.
    #[test]
    fn one_anchor_shares_one_memo() {
        let cache = StorageMemoCache::new();
        assert_eq!(cache.anchor(), None, "no anchor installed yet");

        let anchor = ReadAnchor::identified(100, B256::new([0xaa; 32]));
        let first_fanout = cache.for_anchor(anchor);
        first_fanout.put(Address::ZERO, SLOT, StorageValue::from(42u64));

        let second_fanout = cache.for_anchor(anchor);
        assert!(
            Arc::ptr_eq(&first_fanout, &second_fanout),
            "the same anchor must hand out the same memo"
        );
        assert_eq!(
            second_fanout.get(Address::ZERO, SLOT),
            Some(StorageValue::from(42u64)),
            "the second fan-out reads the first fan-out's cold-miss learning"
        );
        assert_eq!(cache.anchor(), Some(anchor));
    }

    /// The invalidation contract: the anchor moves -> a FRESH memo, and the
    /// retired memo's values are unreachable from the new scope.
    #[test]
    fn an_anchor_move_retires_the_memo() {
        let cache = StorageMemoCache::new();
        let old = cache.for_anchor(ReadAnchor::at(100));
        old.put(Address::ZERO, SLOT, StorageValue::from(42u64));

        let moved = cache.for_anchor(ReadAnchor::at(101));
        assert!(
            !Arc::ptr_eq(&old, &moved),
            "a moved anchor needs a new memo"
        );
        assert_eq!(
            moved.get(Address::ZERO, SLOT),
            None,
            "the new anchor starts cold — the old anchor's values are not carried"
        );
        assert_eq!(cache.anchor(), Some(ReadAnchor::at(101)));
        // The retired memo still holds what it read — for the fan-outs already
        // pinned at ITS anchor, never for the moved one.
        assert_eq!(
            old.get(Address::ZERO, SLOT),
            Some(StorageValue::from(42u64)),
            "the retired memo keeps only its own anchor's values"
        );
    }

    /// A seeded entry — the prefetch lane's intake — serves the read exactly as
    /// a fallback-learned entry does: no forward, one hit.
    #[test]
    fn a_seeded_entry_serves_the_read() {
        let memo = StorageMemo::new();
        assert!(!memo.holds(Address::ZERO, SLOT));
        memo.seed(Address::ZERO, SLOT, StorageValue::from(42u64));
        assert!(
            memo.holds(Address::ZERO, SLOT),
            "the seed installs the entry"
        );
        assert_eq!(
            memo.get(Address::ZERO, SLOT),
            Some(StorageValue::from(42u64))
        );
        assert_eq!(memo.stats(), (1, 0), "a served seed is a hit, never a miss");
    }

    /// `holds` is a pure predicate: the prefetch's skip-if-known gate must not
    /// perturb the hit/miss profile the run itself shapes.
    #[test]
    fn holds_does_not_move_the_counters() {
        let memo = StorageMemo::new();
        memo.seed(Address::ZERO, SLOT, StorageValue::from(7u64));
        assert!(memo.holds(Address::ZERO, SLOT));
        assert!(!memo.holds(Address::ZERO, U256::from(99u64)));
        assert_eq!(memo.stats(), (0, 0), "the gate is a predicate, not a read");
    }

    /// The fence itself: a same-height reorg keeps the anchor's number and
    /// replaces its block. The memo filled on the abandoned block must never
    /// serve a fan-out reading the replacement - the exact staleness a
    /// number-keyed memo cannot see.
    #[test]
    fn a_same_height_reorg_mints_a_fresh_memo() {
        let cache = StorageMemoCache::new();
        let abandoned = ReadAnchor::identified(100, B256::new([0xaa; 32]));
        let replacement = ReadAnchor::identified(100, B256::new([0xbb; 32]));

        let pre_reorg = cache.for_anchor(abandoned);
        let stale_value = StorageValue::from(42u64);
        pre_reorg.put(Address::ZERO, SLOT, stale_value);

        let post_reorg = cache.for_anchor(replacement);
        assert!(
            !Arc::ptr_eq(&pre_reorg, &post_reorg),
            "the replacement block is a different anchor at the same height"
        );
        assert_eq!(
            post_reorg.get(Address::ZERO, SLOT),
            None,
            "the abandoned block's word is unreachable from the replacement block"
        );
        assert_eq!(cache.anchor(), Some(replacement));

        // The replacement's own answers are served, and its fan-outs share
        // them exactly as an ordinary anchor's do.
        let canonical_value = StorageValue::from(7u64);
        post_reorg.put(Address::ZERO, SLOT, canonical_value);
        assert_eq!(
            cache.for_anchor(replacement).get(Address::ZERO, SLOT),
            Some(canonical_value),
            "the same block (number AND hash) keeps sharing its memo"
        );
        assert_eq!(
            pre_reorg.get(Address::ZERO, SLOT),
            Some(stale_value),
            "the abandoned memo still holds only the abandoned block's values"
        );
        assert_ne!(
            post_reorg.get(Address::ZERO, SLOT),
            Some(stale_value),
            "the pre-reorg value is never served at the replacement block"
        );
    }

    /// An unresolved anchor (no hash) cannot be confirmed as the installed
    /// block, so it mints its own memo: the degrade is cold, never stale.
    #[test]
    fn an_unresolved_anchor_never_inherits_a_hashed_memo() {
        let cache = StorageMemoCache::new();
        let warm = cache.for_anchor(ReadAnchor::identified(100, B256::new([0xaa; 32])));
        warm.put(Address::ZERO, SLOT, StorageValue::from(42u64));

        let unresolved = cache.for_anchor(ReadAnchor::at(100));
        assert!(
            !Arc::ptr_eq(&warm, &unresolved),
            "an unidentified anchor is never assumed to be an installed one"
        );
        assert_eq!(
            unresolved.get(Address::ZERO, SLOT),
            None,
            "the unfenced scope starts cold"
        );
    }

    /// A reorg rewind lowers the anchor: the memo filled at the rewound-from
    /// height must never shadow the value the chain serves at the new one.
    #[test]
    fn a_backward_anchor_move_never_shadows_the_new_value() {
        let cache = StorageMemoCache::new();
        let rewound_from = cache.for_anchor(ReadAnchor::at(100));
        rewound_from.put(Address::ZERO, SLOT, StorageValue::from(42u64));

        let rewound_to = cache.for_anchor(ReadAnchor::at(99));
        assert!(
            !Arc::ptr_eq(&rewound_from, &rewound_to),
            "a rewind is an anchor move like any other"
        );
        let fresh_chain_value = StorageValue::from(7u64);
        rewound_to.put(Address::ZERO, SLOT, fresh_chain_value);
        assert_eq!(
            rewound_to.get(Address::ZERO, SLOT),
            Some(fresh_chain_value),
            "the new anchor's own answer is served, not the rewound-from value"
        );
        assert_eq!(
            rewound_from.get(Address::ZERO, SLOT),
            Some(StorageValue::from(42u64)),
            "the retired memo is unchanged (it belongs to the abandoned height)"
        );
    }
}
