//! `StorageMemo` - a PER-CYCLE read memo in front of the RPC fallback.

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

use alloy::primitives::{Address, StorageValue, U256};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Memo hit for this block (the memo is block-scoped; the owner recreates
    /// it when the sim block advances).
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

    /// `(hits, misses)` - the lab counters for the probe logs.
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
