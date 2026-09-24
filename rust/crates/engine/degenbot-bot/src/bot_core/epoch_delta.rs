//! `EpochDelta` — the per-block touched-pool recency ledger.
//!
//! ADR-041 seam-retirement lineage: `DirtySets` + the `EngineSubscriber`
//! pool classification are retired in favor of this type. Log application
//! (`LogDispatcher::dispatch`) records the touched `(HopType, pool_id)`
//! key — the `pool_to_paths` reverse-index key, i.e. an
//! [`AffectedKey`](degenbot_solvers::affected_keys::AffectedKey) — as a
//! BYPRODUCT of `Bot::dispatch_log`, one ledger per block epoch.
//! Affected-path derivation reads the ledger directly at the drain; no
//! subscriber-side classification and no dirty-set machinery survive the
//! cutover.
//!
//! ## Recency buckets
//!
//! The touched set is keyed by the block that touched it: the newest block's
//! bucket is drawn first, older buckets after, and WITHIN a bucket keys are
//! drawn in insertion order (earliest-inserted first). A re-touch PROMOTES
//! the key into the newest bucket — dedup is structural (a key lives in
//! exactly one bucket), and the reverse index locates the old bucket without
//! scanning; the old-bucket removal is order-preserving, so promotion never
//! perturbs the bucket it leaves. The ledger therefore answers "which dirty
//! pools are freshest?" without knowledge of the work items or log volume
//! that produced the dirt: memory is bounded by the number of UNIQUE dirty
//! pools in the retention window.
//!
//! ## Consumption semantics
//!
//! `take_keys` drains atomically (a drain consumes exactly what accumulated
//! so far; later dispatches accumulate into the same ledger for the next
//! drain) and equals a full [`EpochDelta::draw_freshest`] — parity gate).
//! `draw_freshest(budget)` is the capacity-bounded variant a solve cycle uses
//! to take the freshest `budget` keys and RETAIN the rest for a later cycle.
//! A rewind RELABELS the epoch without voiding buckets — matching the retired
//! dirty sets, which were never cleared on reorg (a restored pool re-notifies
//! and re-records; its pre-rewind dirt stays valid because post-rewind
//! re-solves compute against the restored state). `expire_older_than` is the
//! explicit block-window prune the caller drives.

use std::collections::BTreeMap;

use hashbrown::HashMap;
use indexmap::IndexSet;
use parking_lot::Mutex;

use super::epoch::Epoch;
use degenbot_solvers::affected_keys::AffectedKey;

/// The ordered key buckets backing [`EpochDelta`].
///
/// * `buckets` — block → keys touched at that block (newest block = highest
///   `BTreeMap` key). Iterating `.rev()` yields newest-first.
/// * `placement` — key → the block of its bucket. This reverse index locates
///   the old bucket for a re-touch (promotion) without scanning buckets; the
///   removal itself is an order-preserving `shift_remove` (O(n) in that
///   bucket's length), followed by an O(1) insert into the new bucket.
///
/// Data-structure picks (7S4QAG): `indexmap::IndexSet` is the canonical
/// insertion-ordered hash set, and `hashbrown` ships no ordered variant — so
/// the bucket set is `indexmap::IndexSet` while the reverse index stays on the
/// already-vendored `hashbrown::HashMap` (foldhash). Bucket order is block
/// order (`BTreeMap`); WITHIN a bucket it is `IndexSet` insertion order,
/// preserved by every mutation: promotion uses order-preserving
/// `shift_remove`, a fully-drawn bucket is drained with `std::mem::take`
/// (natural order), and a partially-drawn bucket is split with
/// `IndexSet::split_off` so the drawn head and the retained tail each keep
/// order. Both orders are pure functions of the operation sequence
/// (indexmap's hasher never leaks into iteration order), so replayed corpora
/// compare byte-wise.
#[derive(Default)]
struct Ledger {
    buckets: BTreeMap<u64, IndexSet<AffectedKey>>,
    placement: HashMap<AffectedKey, u64>,
}

/// The epoch's touched-pool ledger: keys recorded by log application,
/// consumed by the drain's affected-path derivation.
pub struct EpochDelta {
    epoch: parking_lot::RwLock<Epoch>,
    keys: Mutex<Ledger>,
}

impl EpochDelta {
    /// A ledger minted for `epoch`. Constructed by `Bot` and shared with
    /// the drain seam ([`super::solve_coordinator`]).
    #[must_use]
    pub fn new(epoch: impl Into<Epoch>) -> Self {
        Self {
            epoch: parking_lot::RwLock::new(epoch.into()),
            keys: Mutex::new(Ledger::default()),
        }
    }

    /// The ledger's labeled epoch (bookkeeping metadata only — keys
    /// accumulate across drains regardless of the label).
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        *self.epoch.read()
    }

    /// Relabel the ledger (a rewind or block advance). Buckets are RETAINED:
    /// the touched-set is solve-cursor state, not block-window state — the
    /// retired dirty sets were never cleared on reorg either, and the reorg
    /// coordinator's per-pool restore re-records what it restored. Only
    /// [`expire_older_than`](Self::expire_older_than) prunes buckets.
    pub fn set_epoch(&self, epoch: Epoch) {
        *self.epoch.write() = epoch;
    }

    /// Record one touched key at `block`.
    ///
    /// PROMOTION: a key already present moves into `block`'s bucket (a
    /// re-touch is a priority upgrade; dedup is structural — one entry, in
    /// the newest bucket only). The reverse index locates the old bucket
    /// without scanning; removal uses order-preserving `IndexSet::shift_remove`
    /// (O(n) in that bucket's length) so promotion never perturbs the order of
    /// the keys left behind. Re-recording in the SAME bucket is a no-op
    /// (idempotent, and preserves that bucket's insertion order).
    pub fn record(&self, key: AffectedKey, block: u64) {
        let Ledger { buckets, placement } = &mut *self.keys.lock();
        if let Some(&prev) = placement.get(&key) {
            if prev == block {
                return;
            }
            if let Some(old) = buckets.get_mut(&prev) {
                // `shift_remove` (not `swap_remove`) keeps the remaining keys
                // in insertion order; the bucket is the touched-poorpools
                // set, so its O(n) cost is bounded by tens (hundreds under a
                // registration flood).
                old.shift_remove(&key);
                if old.is_empty() {
                    buckets.remove(&prev);
                }
            }
        }
        buckets.entry(block).or_default().insert(key);
        placement.insert(key, block);
    }

    /// Convenience: record a (hop family, pool id) pair at `block`.
    pub fn record_affected(&self, hop: degenbot_solvers::mixed::HopType, pool_id: u64, block: u64) {
        self.record(AffectedKey::new(hop, pool_id), block);
    }

    /// Draw up to `budget` of the FRESHEST touched keys and RETAIN the rest.
    ///
    /// Draw order is bucket recency (newest block first, older buckets after),
    /// then insertion order WITHIN each bucket: a partial draw takes the
    /// earliest-inserted keys still pending in the bucket and leaves the
    /// later-inserted remainder in its original order for a later cycle. A
    /// fully-covered bucket is drained whole in natural insertion order. A
    /// `budget` of `0` draws nothing; `usize::MAX` (or any budget at least the
    /// pending count) draws everything and is exactly
    /// [`take_keys`](Self::take_keys). Buckets emptied by the draw are
    /// removed.
    #[must_use]
    pub fn draw_freshest(&self, budget: usize) -> Vec<AffectedKey> {
        let mut drawn = Vec::new();
        if budget == 0 {
            return drawn;
        }
        let Ledger { buckets, placement } = &mut *self.keys.lock();
        // Newest block first (`BTreeMap::iter().rev()`), older buckets after.
        for (_block, set) in buckets.iter_mut().rev() {
            let remaining = budget - drawn.len();
            if remaining == 0 {
                break;
            }
            if remaining >= set.len() {
                // The budget covers the rest of this bucket: drain it whole.
                // `mem::take` is O(n) and yields natural insertion order.
                for key in std::mem::take(set) {
                    placement.remove(&key);
                    drawn.push(key);
                }
            } else {
                // Partial drain: take the earliest-inserted `remaining` keys.
                // `split_off` is O(n) and order-preserving on BOTH sides — the
                // drawn head keeps insertion order and the retained tail keeps
                // its original order for the next cycle.
                let rest = set.split_off(remaining);
                for key in std::mem::take(set) {
                    placement.remove(&key);
                    drawn.push(key);
                }
                *set = rest;
            }
        }
        buckets.retain(|_, set| !set.is_empty());
        drawn
    }

    /// Prune every bucket BELOW `oldest_block`, returning the number of keys
    /// removed (the caller turns the count into telemetry). Buckets at or
    /// above the cutoff are untouched.
    pub fn expire_older_than(&self, oldest_block: u64) -> usize {
        let Ledger { buckets, placement } = &mut *self.keys.lock();
        // Distinct stale blocks only (`range` = O(log n + k)).
        let stale: Vec<u64> = buckets
            .range(..oldest_block)
            .map(|(&block, _)| block)
            .collect();
        let mut pruned = 0;
        for block in stale {
            if let Some(set) = buckets.remove(&block) {
                pruned += set.len();
                for key in set {
                    placement.remove(&key);
                }
            }
        }
        pruned
    }

    /// Atomically take every touched key (drain consumption), leaving the
    /// ledger empty for the next solve cycle — the retired
    /// `DirtySets::take_all` semantics, keys now driving the derivation. By
    /// definition a full [`draw_freshest`](Self::draw_freshest), so the two
    /// consumption paths stay in parity. Keys come out in recency-bucket order
    /// (newest block first, insertion order within each bucket) — NOT sorted
    /// order.
    #[must_use]
    pub fn take_keys(&self) -> Vec<AffectedKey> {
        self.draw_freshest(usize::MAX)
    }

    /// Read WITHOUT consuming — the parity gate's non-destructive read side.
    /// Sorted by key so the bytes are independent of bucket order.
    #[must_use]
    pub fn snapshot_keys(&self) -> Vec<AffectedKey> {
        let mut keys: Vec<AffectedKey> = self
            .keys
            .lock()
            .buckets
            .values()
            .flat_map(|set| set.iter().copied())
            .collect();
        keys.sort_unstable();
        keys
    }

    /// Returns `true` if no touched keys are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.lock().buckets.is_empty()
    }
}

impl std::fmt::Debug for EpochDelta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochDelta")
            .field("epoch", &self.epoch())
            .field("pending_keys", &self.snapshot_keys().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use degenbot_solvers::mixed::HopType;

    fn key(hop: HopType, pool_id: u64) -> AffectedKey {
        AffectedKey::new(hop, pool_id)
    }

    #[test]
    fn records_and_takes_atomically() {
        let delta = EpochDelta::new(42u64);
        delta.record_affected(HopType::V2, 7, 100);
        delta.record_affected(HopType::V2, 7, 100); // dedup
        delta.record_affected(HopType::V3, 9, 100);
        assert_eq!(delta.snapshot_keys().len(), 2);
        assert!(!delta.is_empty());
        let taken = delta.take_keys();
        assert_eq!(taken.len(), 2);
        assert!(delta.is_empty(), "take clears the ledger");
        // A later drain after new dispatches sees only the new keys.
        delta.record_affected(HopType::V4, 3, 101);
        assert_eq!(delta.take_keys().len(), 1);
    }

    #[test]
    fn rewind_relables_and_keeps_keys() {
        let delta = EpochDelta::new(10u64);
        delta.record_affected(HopType::V2, 1, 10);
        delta.set_epoch(Epoch::at(10).rewind_to(9));
        assert_eq!(delta.epoch(), Epoch::with_generation(9, 1));
        // Keys survive the relabel — solve-cursor state, not block-window state.
        assert_eq!(delta.take_keys().len(), 1);
    }

    // --- 7S4QAG recency-ledger contract (red-first) ---

    #[test]
    fn re_record_at_a_later_block_promotes_to_the_newest_bucket() {
        let delta = EpochDelta::new(1u64);
        let k = key(HopType::V2, 7);
        delta.record(k, 100);
        delta.record(k, 101); // promotion: re-touch is a priority upgrade
                              // Exactly once, in the newest bucket only.
        assert_eq!(delta.snapshot_keys(), vec![k]);
        assert_eq!(delta.draw_freshest(usize::MAX), vec![k]);
        // Nothing survives below the newest bucket: expiry at 101 prunes nothing.
        assert_eq!(delta.expire_older_than(101), 0);
    }

    #[test]
    fn draw_exhausts_newest_bucket_first_and_retains_the_remainder() {
        let delta = EpochDelta::new(1u64);
        let old_a = key(HopType::V2, 1);
        let old_b = key(HopType::V2, 2);
        let new_a = key(HopType::V3, 3);
        let new_b = key(HopType::V3, 4);
        delta.record(old_a, 10);
        delta.record(old_b, 10);
        delta.record(new_a, 11);
        delta.record(new_b, 11);
        // Budget 1: the newest bucket first (insertion order within the bucket).
        assert_eq!(delta.draw_freshest(1), vec![new_a]);
        // Budget 2 more: the rest of the newest bucket, then the older bucket.
        assert_eq!(delta.draw_freshest(2), vec![new_b, old_a]);
        // The remainder is RETAINED for a later cycle.
        assert_eq!(delta.draw_freshest(10), vec![old_b]);
        assert!(delta.is_empty());
        assert_eq!(delta.draw_freshest(10), Vec::new());
    }

    #[test]
    fn partial_draw_and_remainder_follow_insertion_order() {
        // A bucket wider than any budget: budget 2 must take the two
        // EARLIEST-inserted keys [a, b] and retain [c, d] untouched.
        let delta = EpochDelta::new(1u64);
        let a = key(HopType::V2, 1);
        let b = key(HopType::V2, 2);
        let c = key(HopType::V2, 3);
        let d = key(HopType::V2, 4);
        for k in [a, b, c, d] {
            delta.record(k, 10);
        }
        assert_eq!(delta.draw_freshest(2), vec![a, b]);
        // The retained remainder keeps insertion order, so the follow-up full
        // draw continues where the partial draw stopped.
        assert_eq!(delta.draw_freshest(usize::MAX), vec![c, d]);
        assert!(delta.is_empty());
    }

    #[test]
    fn promoting_a_middle_key_preserves_old_bucket_order() {
        let delta = EpochDelta::new(1u64);
        let a = key(HopType::V2, 1);
        let b = key(HopType::V2, 2);
        let c = key(HopType::V2, 3);
        let d = key(HopType::V2, 4);
        for k in [a, b, c, d] {
            delta.record(k, 10);
        }
        // Promote a MIDDLE key into a newer bucket.
        delta.record(c, 11);
        // The newest bucket now holds only the promoted key.
        assert_eq!(delta.draw_freshest(1), vec![c]);
        // The old bucket keeps insertion order minus the promoted key.
        assert_eq!(delta.draw_freshest(usize::MAX), vec![a, b, d]);
    }

    #[test]
    fn full_draw_equals_take_keys() {
        // Two identically-fed ledgers: a full draw and `take_keys` are the
        // same consumption path and must agree byte-wise.
        let a = EpochDelta::new(1u64);
        let b = EpochDelta::new(1u64);
        for delta in [&a, &b] {
            delta.record(key(HopType::V2, 1), 10);
            delta.record(key(HopType::V3, 2), 11);
            delta.record(key(HopType::V2, 1), 12); // promote mid-corpus
            delta.record(key(HopType::V4, 3), 10);
        }
        let drawn = a.draw_freshest(usize::MAX);
        let taken = b.take_keys();
        assert_eq!(drawn, taken);
        assert_eq!(drawn.len(), 3);
        assert!(a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn rewind_retains_buckets_then_expiry_prunes_below_cutoff() {
        let delta = EpochDelta::new(10u64);
        let old = key(HopType::V2, 1);
        let fresh = key(HopType::V2, 2);
        delta.record(old, 10);
        delta.record(fresh, 12);
        delta.set_epoch(Epoch::at(10).rewind_to(9));
        assert_eq!(delta.epoch(), Epoch::with_generation(9, 1));
        // Buckets survive the relabel (solve-cursor state, not block-window state).
        assert_eq!(delta.snapshot_keys(), vec![old, fresh]);
        // Expiry prunes ONLY below the cutoff and counts what it pruned.
        assert_eq!(delta.expire_older_than(12), 1);
        assert_eq!(delta.snapshot_keys(), vec![fresh]);
        assert_eq!(delta.expire_older_than(12), 0); // idempotent
    }
}
