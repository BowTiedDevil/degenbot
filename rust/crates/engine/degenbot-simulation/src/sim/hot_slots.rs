//! The capture-learned hot-slot set — the history half of the storage
//! prewarm.
//!
//! The path-derived prefetch ([`crate::sim::prefetch`]) seeds the per-anchor
//! [`StorageMemo`](crate::sim::StorageMemo) with every slot a candidate's path
//! *guarantees* — a derivable subset. What it cannot derive is the arbitrary
//! part: an ERC20 `balanceOf` word lives at the token's own mapping base (not
//! the path's), and a V3/V4 pool's per-tick words are unbounded. Those slots
//! are not predictable from the path — but they are predictable from HISTORY:
//! the bundle capture already records the exact `(address, slot)` set every
//! settled candidate touched ([`Evm2SequenceCapture::access_list`]).
//!
//! This store is that history, per session: `address -> {slot -> last touched}`.
//! The next fan-out asks it for the slots under the addresses its paths name
//! ([`crate::sim::prefetch::path_prefetch_addresses`]) and unions them with the
//! derivation — one batch, never two.
//!
//! # Values never come from here
//!
//! The store holds no values, only the *identity* of a slot. Every prewarm read
//! goes to the node at the fan-out's own anchor, exactly like the path-derived
//! lane: a prediction that has drifted costs one wasted batch member, never a
//! wrong read.
//!
//! # Bounds and decay
//!
//! Unbounded growth is the failure mode of every learned set, so the store is
//! bounded twice over — [`MAX_HOT_SLOTS_PER_ADDRESS`] per address (a pool's
//! steady-state read set is a handful of words; a contract that keeps minting
//! new slots evicts its coldest) and [`MAX_HOT_ADDRESSES`] overall (the
//! coldest address goes when the map is full). Decay is by block height: a
//! slot no capture has touched for [`HOT_SLOT_DECAY_BLOCKS`] blocks drops out
//! of the next query (per-pool drift — a tick word that stops being read once
//! the price walks away — leaves the set on its own).
//!
//! # Scope
//!
//! The store is session-scoped and cross-block (unlike the per-anchor storage
//! memo it feeds): its whole point is to carry what one anchor taught into the
//! next. It therefore lives on [`WarmCodeCacheInner`], the engine's
//! session-scoped shared-read-cache owner, beside the memo cache.
//!
//! [`Evm2SequenceCapture::access_list`]: crate::sim::evm2::session::Evm2SequenceCapture

use alloy::primitives::map::AddressHashMap;
use alloy::primitives::{Address, U256};
use alloy::rpc::types::eth::AccessList;
use parking_lot::Mutex;
use std::collections::BTreeMap;

/// How many blocks a slot stays hot after the last capture touched it. A slot
/// whose last touch is `anchor - HOT_SLOT_DECAY_BLOCKS` (or older) is decayed
/// out of the next prewarm.
pub const HOT_SLOT_DECAY_BLOCKS: u64 = 5;

/// The most slots one address may hold. A pool's steady-state read set is a
/// handful of words; eviction is coldest-first.
pub const MAX_HOT_SLOTS_PER_ADDRESS: usize = 32;

/// The most addresses the store tracks. The batch's paths name a few pools and
/// tokens each, so this is a multi-block working set, not the chain.
pub const MAX_HOT_ADDRESSES: usize = 4096;

/// `slot -> the anchor whose capture last touched it`.
type SlotAges = BTreeMap<U256, u64>;

#[derive(Default)]
struct Inner {
    rows: AddressHashMap<SlotAges>,
}

impl Inner {
    /// One slot's live span: a touch at `last` is live while fewer than
    /// [`HOT_SLOT_DECAY_BLOCKS`] blocks have passed. A BACKWARD anchor move (a
    /// reorg rewind) cannot make an entry look fresher than it is —
    /// `saturating_sub` reads a negative age as zero — so a rewound anchor
    /// keeps the set, and the next forward move resumes the decay.
    fn is_live(last: u64, anchor: u64) -> bool {
        anchor.saturating_sub(last) < HOT_SLOT_DECAY_BLOCKS
    }

    /// Drop `row`'s decayed slots.
    fn prune_row(row: &mut SlotAges, anchor: u64) {
        row.retain(|_, last| Self::is_live(*last, anchor));
    }

    /// Record one touch at `anchor`, enforcing the per-address bound.
    fn touch(row: &mut SlotAges, slot: U256, anchor: u64) {
        // A rewind must not age an entry: the last-touch anchor is monotone.
        let last = row.get(&slot).copied().unwrap_or(anchor).max(anchor);
        row.insert(slot, last);
        while row.len() > MAX_HOT_SLOTS_PER_ADDRESS {
            // Coldest-first: the smallest last-touch anchor goes (ties break on
            // the slot key, so eviction is deterministic).
            let Some(coldest) = row
                .iter()
                .min_by_key(|(slot, last)| (**last, **slot))
                .map(|(slot, _)| *slot)
            else {
                return;
            };
            row.remove(&coldest);
        }
    }
}

/// The session's capture-learned hot-slot set (see the module docs).
#[derive(Default)]
pub struct HotSlotStore {
    inner: Mutex<Inner>,
}

impl HotSlotStore {
    /// An empty store: nothing learned yet (the first captures fill it).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one `(address, slot)` touch by a capture that read at `anchor`.
    pub fn record(&self, anchor: u64, address: Address, slot: U256) {
        let mut inner = self.inner.lock();
        ensure_address(&mut inner, address);
        let Some(row) = inner.rows.get_mut(&address) else {
            return;
        };
        Inner::prune_row(row, anchor);
        Inner::touch(row, slot, anchor);
    }

    /// Record a completed capture's access list — the learning path: the
    /// slots the settled candidate actually touched, at the anchor its reads
    /// were pinned to.
    pub fn record_capture(&self, anchor: u64, access_list: &AccessList) {
        let mut inner = self.inner.lock();
        for item in &access_list.0 {
            if item.storage_keys.is_empty() {
                continue;
            }
            ensure_address(&mut inner, item.address);
            let Some(row) = inner.rows.get_mut(&item.address) else {
                continue;
            };
            Inner::prune_row(row, anchor);
            for key in &item.storage_keys {
                Inner::touch(row, U256::from_be_bytes(key.0), anchor);
            }
        }
    }

    /// The live slots under `addresses`, in the caller's address order and
    /// ascending slot order within each address — the prewarm's learned
    /// complement. Decayed slots are dropped as they are observed, so a query
    /// after the decay window returns nothing.
    #[must_use]
    pub fn slots_for(
        &self,
        anchor: u64,
        addresses: impl IntoIterator<Item = Address>,
    ) -> Vec<(Address, U256)> {
        let mut inner = self.inner.lock();
        let mut slots = Vec::new();
        for address in addresses {
            let Some(row) = inner.rows.get_mut(&address) else {
                continue;
            };
            Inner::prune_row(row, anchor);
            if row.is_empty() {
                inner.rows.remove(&address);
                continue;
            }
            for slot in row.keys() {
                slots.push((address, *slot));
            }
        }
        slots
    }

    /// The number of live slots the store holds at `anchor` (prunes as it
    /// counts — the diagnostic seam, not a per-fan-out read).
    #[must_use]
    pub fn live_slot_count(&self, anchor: u64) -> usize {
        let mut inner = self.inner.lock();
        let mut live = 0usize;
        let addresses: Vec<Address> = inner.rows.keys().copied().collect();
        for address in addresses {
            let Some(row) = inner.rows.get_mut(&address) else {
                continue;
            };
            Inner::prune_row(row, anchor);
            if row.is_empty() {
                inner.rows.remove(&address);
            } else {
                live += row.len();
            }
        }
        live
    }
}

/// Make room for `address` (evicting the coldest row when the map is full) so
/// the caller's `get_mut` succeeds.
fn ensure_address(inner: &mut Inner, address: Address) {
    if inner.rows.contains_key(&address) {
        return;
    }
    if inner.rows.len() >= MAX_HOT_ADDRESSES {
        // Coldest address first: its row's newest touch is the smallest.
        let coldest = inner
            .rows
            .iter()
            .min_by_key(|(candidate, row)| (row.values().copied().max().unwrap_or(0), **candidate))
            .map(|(candidate, _)| *candidate);
        if let Some(coldest) = coldest {
            inner.rows.remove(&coldest);
        }
    }
    inner.rows.insert(address, SlotAges::new());
}

#[cfg(test)]
#[expect(clippy::expect_used)] // test-only bounds: the consts are small by construction
mod tests {
    use super::*;
    use alloy::primitives::B256;
    use alloy::rpc::types::eth::{AccessList, AccessListItem};

    const POOL: Address = Address::new([0x01; 20]);
    const TOKEN: Address = Address::new([0x02; 20]);

    fn key(slot: u64) -> B256 {
        B256::new(U256::from(slot).to_be_bytes::<32>())
    }

    fn capture(entries: &[(Address, &[u64])]) -> AccessList {
        AccessList::from(
            entries
                .iter()
                .map(|(address, slots)| AccessListItem {
                    address: *address,
                    storage_keys: slots.iter().map(|slot| key(*slot)).collect(),
                })
                .collect::<Vec<_>>(),
        )
    }

    fn addr(index: u64) -> Address {
        let mut bytes = [0u8; 20];
        bytes[12..].copy_from_slice(&index.to_be_bytes());
        Address::new(bytes)
    }

    /// The learning path's intake: a completed capture's access list is the
    /// exact `(address, slot)` set, and the store answers it back for the
    /// addresses the next batch names.
    #[test]
    fn a_capture_teaches_its_touched_slots() {
        let store = HotSlotStore::new();
        store.record_capture(100, &capture(&[(POOL, &[2, 4]), (TOKEN, &[0xbeef])]));
        assert_eq!(
            store.slots_for(100, [POOL, TOKEN]),
            vec![
                (POOL, U256::from(2u64)),
                (POOL, U256::from(4u64)),
                (TOKEN, U256::from(0xbeefu64)),
            ],
            "the capture's slots come back in address order, slots ascending"
        );
        assert!(
            store.slots_for(100, [Address::new([0x09; 20])]).is_empty(),
            "an address no capture touched teaches nothing"
        );
    }

    /// A re-touch refreshes the slot's decay clock: the same slot seen in a
    /// later capture survives past the first sighting's decay window.
    #[test]
    fn a_repeated_touch_refreshes_the_decay_clock() {
        let store = HotSlotStore::new();
        store.record(100, POOL, U256::from(7u64));
        store.record(101, POOL, U256::from(7u64));
        assert_eq!(
            store.slots_for(105, [POOL]),
            vec![(POOL, U256::from(7u64))],
            "the touch at 101 keeps the slot hot at 105 (the touch at 100 would not)"
        );
        assert_eq!(
            store.slots_for(105, [POOL]),
            vec![(POOL, U256::from(7u64))],
            "a duplicate sighting does not duplicate the entry"
        );
    }

    /// Drift decay: a slot no capture has touched for `HOT_SLOT_DECAY_BLOCKS`
    /// blocks drops out of the prewarm; the last live block is
    /// `last + HOT_SLOT_DECAY_BLOCKS - 1`.
    #[test]
    fn an_untouched_slot_decays_out() {
        let store = HotSlotStore::new();
        let touched_at = 100u64;
        store.record(touched_at, POOL, U256::from(8u64));
        for anchor in touched_at..touched_at + HOT_SLOT_DECAY_BLOCKS {
            assert!(
                !store.slots_for(anchor, [POOL]).is_empty(),
                "a slot touched at {touched_at} is live at {anchor}"
            );
        }
        assert!(
            store
                .slots_for(touched_at + HOT_SLOT_DECAY_BLOCKS, [POOL])
                .is_empty(),
            "the slot decays out once the window has passed"
        );
        assert_eq!(
            store.live_slot_count(touched_at + HOT_SLOT_DECAY_BLOCKS),
            0,
            "a decayed slot is not counted as live"
        );
    }

    /// A reorg rewind lowers the anchor: the set does not go stale on the way
    /// down, and the forward decay resumes from the last touch.
    #[test]
    fn a_rewind_does_not_age_a_slot() {
        let store = HotSlotStore::new();
        store.record(100, POOL, U256::from(9u64));
        assert_eq!(
            store.slots_for(99, [POOL]),
            vec![(POOL, U256::from(9u64))],
            "the rewound anchor still sees the slot (age saturates at zero)"
        );
        assert!(
            store
                .slots_for(100 + HOT_SLOT_DECAY_BLOCKS, [POOL])
                .is_empty(),
            "the forward decay window counts from the last touch"
        );
    }

    /// The per-address bound: a contract that keeps minting new slots evicts
    /// its coldest (the oldest slot key on a tie) rather than growing without
    /// limit.
    #[test]
    fn the_per_address_bound_evicts_the_coldest_slot() {
        let store = HotSlotStore::new();
        let bound = u64::try_from(MAX_HOT_SLOTS_PER_ADDRESS).expect("a small bound");
        for slot in 0..=bound {
            store.record(100, POOL, U256::from(slot));
        }
        let held = store.slots_for(100, [POOL]);
        assert_eq!(held.len(), MAX_HOT_SLOTS_PER_ADDRESS);
        assert!(
            !held.contains(&(POOL, U256::ZERO)),
            "the coldest slot (the first touched) is evicted"
        );
        assert!(
            held.contains(&(POOL, U256::from(bound))),
            "the newest slot is retained"
        );
    }

    /// The address bound: the store tracks a bounded working set, evicting the
    /// coldest address when it is full.
    #[test]
    fn the_address_bound_evicts_the_coldest_address() {
        let store = HotSlotStore::new();
        let bound = u64::try_from(MAX_HOT_ADDRESSES).expect("a small bound");
        for index in 0..bound {
            store.record(100, addr(index), U256::from(1u64));
        }
        store.record(100, addr(bound), U256::from(1u64));
        assert_eq!(
            store.live_slot_count(100),
            MAX_HOT_ADDRESSES,
            "the store stays at its bound after the overflow insert"
        );
        assert!(
            store.slots_for(100, [addr(0)]).is_empty(),
            "the coldest address (the first touched) is evicted"
        );
        assert_eq!(
            store.slots_for(100, [addr(bound)]).len(),
            1,
            "the new address is admitted"
        );
    }
}
