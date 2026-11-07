//! Bounded reuse of empty transaction slot maps. No transaction state is retained.

use super::{StorageOverlay, StorageSlot};
use alloc::vec::Vec;
use alloy_primitives::map::U256Map;

const MAX_MAPS: usize = 32;
const MAX_MAP_CAPACITY: usize = 4_096;
const MAX_TOTAL_CAPACITY: usize = 16_384;

/// Retains only empty allocations. Active transaction maps and the accepted cache are separate.
#[derive(Debug, Default)]
pub(super) struct StoragePool {
    maps: Vec<U256Map<StorageSlot>>,
    capacity: usize,
}

impl StoragePool {
    pub(super) fn take(&mut self) -> U256Map<StorageSlot> {
        match self.maps.pop() {
            Some(map) => {
                self.capacity -= map.capacity();
                map
            }
            None => U256Map::default(),
        }
    }

    /// Retains one empty account-local slot map.
    pub(super) fn clear_overlay(&mut self, overlay: &mut StorageOverlay) {
        let mut slots = core::mem::take(&mut overlay.slots);
        overlay.wiped = false;
        let capacity = slots.capacity();
        if capacity == 0
            || capacity > MAX_MAP_CAPACITY
            || self.maps.len() == MAX_MAPS
            || capacity > MAX_TOTAL_CAPACITY - self.capacity
        {
            return;
        }
        slots.clear();
        let capacity = slots.capacity();
        if capacity > MAX_MAP_CAPACITY || capacity > MAX_TOTAL_CAPACITY - self.capacity {
            return;
        }
        self.capacity += capacity;
        self.maps.push(slots);
    }
}
