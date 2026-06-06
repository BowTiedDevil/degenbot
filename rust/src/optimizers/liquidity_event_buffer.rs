//! Dual-buffer for liquidity events awaiting pool registration.
//!
//! V3 and V4 engines both maintain two event buffers:
//! - **Backfill buffer**: events from the snapshot gap (never expired)
//! - **Pump buffer**: events from the live WS subscription (expired by age)
//!
//! The buffer owns storage and lifecycle (expiry, flush, count).
//! Application of drained events is the engine's responsibility —
//! it needs to call `update_tick_liquidity` and invalidate caches.
//!
//! # Generics
//!
//! - `K`: Pool identifier type (`Address` for V3, `(Address, PoolId)` for V4)
//! - `U`: Buffered update type (`BufferedV3LiquidityUpdate` or `BufferedV4LiquidityUpdate`)

use std::collections::HashMap;
use std::hash::Hash;

/// A dual-buffer for liquidity events awaiting pool registration.
///
/// # Invariants
///
/// - Backfill buffer is never expired (covers a fixed block range).
/// - Pump buffer events are expired when older than `max_age` blocks.
/// - Both buffers are keyed by pool identity (address or `pool_id`).
#[derive(Debug)]
pub(crate) struct LiquidityEventBuffer<K, U>
where
    K: Eq + Hash + Clone,
{
    /// Buffered liquidity updates from the backfill phase
    /// (`snapshot_block+1` to `first_ws_block-1`).
    /// Never expired — covers a fixed block range and drains
    /// pool-by-pool during `build_paths`.
    backfill: HashMap<K, Vec<U>>,

    /// Buffered liquidity updates from the WS pump phase
    /// (`first_ws_block` onward).
    /// Expired normally via `expire`.
    pump: HashMap<K, Vec<U>>,

    /// Maximum age (in blocks) for pump buffer events.
    /// `None` means unbounded.
    max_age: Option<u64>,
}

impl<K, U> LiquidityEventBuffer<K, U>
where
    K: Eq + Hash + Clone,
{
    /// Create a new empty buffer with unbounded pump event age.
    pub(crate) fn new() -> Self {
        Self {
            backfill: HashMap::new(),
            pump: HashMap::new(),
            max_age: None,
        }
    }

    /// Buffer a liquidity update from the backfill phase.
    pub(crate) fn buffer_backfill(&mut self, key: K, update: U) {
        self.backfill.entry(key).or_default().push(update);
    }

    /// Buffer a liquidity update from the pump (live WS) phase.
    pub(crate) fn buffer_pump(&mut self, key: K, update: U) {
        self.pump.entry(key).or_default().push(update);
    }

    /// Drain and return all backfill events for a pool key.
    ///
    /// Returns `None` if no backfill events exist for this key.
    /// The caller is responsible for applying the returned updates
    /// to the pool's tick data.
    pub(crate) fn drain_backfill(&mut self, key: &K) -> Option<Vec<U>> {
        self.backfill.remove(key)
    }

    /// Drain and return all pump events for a pool key.
    ///
    /// Returns `None` if no pump events exist for this key.
    /// The caller is responsible for applying the returned updates
    /// to the pool's tick data.
    pub(crate) fn drain_pump(&mut self, key: &K) -> Option<Vec<U>> {
        self.pump.remove(key)
    }

    /// Set the maximum age (in blocks) for pump buffer events.
    ///
    /// `None` means unbounded (no automatic expiry).
    pub(crate) fn set_max_age(&mut self, max_age: Option<u64>) {
        self.max_age = max_age;
    }

    /// Return the total number of buffered events for a pool key
    /// (both backfill and pump).
    pub(crate) fn event_count(&self, key: &K) -> usize {
        let backfill = self.backfill.get(key).map_or(0, Vec::len);
        let pump = self.pump.get(key).map_or(0, Vec::len);
        backfill + pump
    }

    /// Discard all buffered events for all pools.
    ///
    /// Frees memory. Called when the operator knows that certain pools
    /// will never be registered.
    pub(crate) fn flush(&mut self) {
        self.backfill.clear();
        self.pump.clear();
    }

    /// Expire pump buffer events whose `block_number` is older than
    /// `current_block - max_age`.
    ///
    /// If `max_age` is `None`, this is a no-op.
    /// Backfill buffer is never expired.
    pub(crate) fn expire(&mut self, current_block: u64)
    where
        U: LiquidityEvent,
    {
        let Some(max_age) = self.max_age else {
            return;
        };

        let cutoff = current_block.saturating_sub(max_age);

        for events in self.pump.values_mut() {
            events.retain(|ev| ev.block_number() >= cutoff);
        }

        self.pump.retain(|_, events| !events.is_empty());
    }

    /// Check whether there are any buffered events at all.
    #[allow(dead_code)] // Used by future callers and tests
    pub(crate) fn is_empty(&self) -> bool {
        self.backfill.is_empty() && self.pump.is_empty()
    }

    /// Check whether the pump buffer contains events for a key.
    #[cfg(test)]
    pub(crate) fn pump_contains_key(&self, key: &K) -> bool {
        self.pump.contains_key(key)
    }

    /// Return the number of pump buffer events for a key.
    #[cfg(test)]
    pub(crate) fn pump_event_count(&self, key: &K) -> usize {
        self.pump.get(key).map_or(0, Vec::len)
    }

    /// Return the total number of keys in the pump buffer.
    #[cfg(test)]
    pub(crate) fn pump_key_count(&self) -> usize {
        self.pump.len()
    }

    /// Check whether the pump buffer is empty.
    #[cfg(test)]
    pub(crate) fn pump_is_empty(&self) -> bool {
        self.pump.is_empty()
    }
}

impl<K, U> Default for LiquidityEventBuffer<K, U>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for buffered liquidity events that support expiry by block number.
pub(crate) trait LiquidityEvent {
    /// The block number at which this event occurred.
    fn block_number(&self) -> u64;
}
