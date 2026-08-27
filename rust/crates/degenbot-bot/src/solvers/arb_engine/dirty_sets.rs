use std::collections::HashSet;

use parking_lot::Mutex;

use super::HopType;

/// Shared dirty-pool-key sets, co-owned by the engine and its subscriber.
///
/// RAYPAR engine-shard T3 (C42WKO): the subscriber writes (`insert`) without
/// taking the engine `Mutex` — previously `on_pool_state_updated` locked the
/// engine just to insert one `u64` into a `HashSet`, parking behind every drain.
/// Now it reads `core` directly (via a shared `Arc<StateLock<BotState>>`)
/// and writes to these sets under a short per-set lock. The drain reads
/// (`take_all`) swaps the sets out atomically.
pub(crate) struct DirtySets {
    v2: Mutex<HashSet<u64>>,
    v3: Mutex<HashSet<u64>>,
    v4: Mutex<HashSet<u64>>,
}

impl DirtySets {
    #[must_use]
    pub fn new() -> Self {
        Self {
            v2: Mutex::new(HashSet::new()),
            v3: Mutex::new(HashSet::new()),
            v4: Mutex::new(HashSet::new()),
        }
    }

    /// Insert `pool_id` into the dirty set for `hop_type`.
    pub fn insert(&self, pool_id: u64, hop_type: HopType) {
        let target = match hop_type {
            HopType::V2 => &self.v2,
            HopType::V3 => &self.v3,
            HopType::V4 => &self.v4,
            _ => return, // Non-pool hop types are never dirtied.
        };
        target.lock().insert(pool_id);
    }

    /// Atomically take all dirty sets, leaving them empty for the next drain.
    #[must_use]
    pub fn take_all(&self) -> (HashSet<u64>, HashSet<u64>, HashSet<u64>) {
        let v2 = std::mem::take(&mut *self.v2.lock());
        let v3 = std::mem::take(&mut *self.v3.lock());
        let v4 = std::mem::take(&mut *self.v4.lock());
        (v2, v3, v4)
    }

    /// Returns `true` if all three dirty sets are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.v2.lock().is_empty() && self.v3.lock().is_empty() && self.v4.lock().is_empty()
    }
}

impl Default for DirtySets {
    fn default() -> Self {
        Self::new()
    }
}
