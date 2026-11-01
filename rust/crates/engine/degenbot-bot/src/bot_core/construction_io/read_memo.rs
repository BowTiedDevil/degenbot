//! The block-scoped construction read memo.
//!
//! Construction reads repeat: the crawl builds many units against the same
//! pool at the same block, and several seams (the builder's immutable batch,
//! the driver's own verbs, the DB-seed reconciliation, the lazy tick-word
//! backfill) ask the same `(to, calldata)` question more than once. The answer
//! is a pure function of the block it was asked at, so the second ask is a
//! chain round-trip the crawl pays for nothing — the crawl is latency-bound,
//! and the repeats dominate its traffic.
//!
//! This memo is the construction seam's memory of those answers. Its sibling
//! is the sim side's storage memo: same validity domain (a pinned block), but
//! this one fronts `eth_call` for construction/refresh, never the simulation
//! path.
//!
//! # Validity contract
//!
//! An `eth_call` return is a function of `(to, calldata, block)`. The key
//! carries all three, so an entry is served ONLY to a read that names the same
//! block it was recorded at — a block move is a MISS and goes to the chain
//! again, never a stale hit. A read with no block (`latest`) has no block to
//! key on (the node resolves the head at execution time), so it is never
//! recorded and never served.
//!
//! # Bounds
//!
//! Entries live per block epoch, and only the newest epochs are retained: the
//! crawl resolves one block per unit and a couple of units are in flight, so
//! an older epoch's answers are never asked for again. A hard entry cap
//! backstops the epoch window, so a pathological block cannot grow the memo
//! without bound.

use std::collections::BTreeMap;

use alloy::primitives::{Address, Bytes};
use hashbrown::HashMap;
use parking_lot::Mutex;

/// The retained block epochs. A unit resolves its own block, so two units in
/// flight can disagree by a block; anything older than that window is never
/// asked for again.
const DEFAULT_MAX_EPOCHS: usize = 2;

/// The retained entries across all epochs. The largest legitimate single-block
/// working set is a DB-seed reconciliation sweep (one word per initialized tick
/// row — thousands for a fine-spacing pool), so this leaves headroom over it
/// while keeping the memo in the low megabytes.
const DEFAULT_MAX_ENTRIES: usize = 16 * 1024;

/// The construction read memo: `(block, to, calldata) -> return bytes`, valid
/// for exactly the block its key names.
///
/// Held behind an `Arc` on the [`ConstructionIo`](super::ConstructionIo)
/// handle, so every construction path in one session — the builder's reads,
/// the tick-word transports attached per pool, the driver's own verbs — shares
/// one memory instead of one per handle.
#[derive(Debug)]
pub struct ReadMemo {
    inner: Mutex<MemoInner>,
    max_epochs: usize,
    max_entries: usize,
}

#[derive(Debug, Default)]
struct MemoInner {
    /// One map per block epoch. A block's entries are only ever served to a
    /// read naming that block.
    epochs: BTreeMap<u64, HashMap<(Address, Bytes), Bytes>>,
    /// The live entry count across every epoch (an eviction-probe counter, so
    /// the cap check never walks the maps).
    entries: usize,
}

impl Default for ReadMemo {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadMemo {
    /// The session memo with the default bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bounds(DEFAULT_MAX_EPOCHS, DEFAULT_MAX_ENTRIES)
    }

    /// A memo with explicit bounds (the construction-time test knob).
    ///
    /// `max_epochs` blocks are retained; `max_entries` caps the total entries
    /// across them.
    #[must_use]
    pub fn with_bounds(max_epochs: usize, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(MemoInner::default()),
            max_epochs: max_epochs.max(1),
            max_entries: max_entries.max(1),
        }
    }

    /// The recorded return for `(block, to, calldata)`, when this memo holds
    /// one. A hit is always a same-block hit by construction.
    #[must_use]
    pub fn get(&self, block: u64, to: Address, data: &Bytes) -> Option<Bytes> {
        self.inner
            .lock()
            .epochs
            .get(&block)
            .and_then(|epoch| epoch.get(&(to, data.clone())).cloned())
    }

    /// Record a successful read. A block older than the retained window is
    /// recorded only if the window still has room; an epoch past the entry cap
    /// drops the whole memory (a sweep far larger than any legitimate block's
    /// working set is not worth an LRU).
    pub fn insert(&self, block: u64, to: Address, data: &Bytes, value: &Bytes) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        if inner.entries >= self.max_entries {
            inner.epochs.clear();
            inner.entries = 0;
        }
        let recorded = inner
            .epochs
            .entry(block)
            .or_default()
            .insert((to, data.clone()), value.clone());
        if recorded.is_none() {
            inner.entries += 1;
        }
        while inner.epochs.len() > self.max_epochs {
            let Some(oldest) = inner.epochs.keys().next().copied() else {
                break;
            };
            if let Some(dropped) = inner.epochs.remove(&oldest) {
                inner.entries -= dropped.len();
            }
        }
    }

    /// The live entry count (bounded by the entry cap).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().entries
    }

    /// Whether the memo holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
