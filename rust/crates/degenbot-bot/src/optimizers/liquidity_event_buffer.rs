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
pub struct LiquidityEventBuffer<K, U>
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
    #[must_use]
    pub fn new() -> Self {
        Self {
            backfill: HashMap::new(),
            pump: HashMap::new(),
            max_age: None,
        }
    }

    /// Buffer a liquidity update from the backfill phase.
    pub fn buffer_backfill(&mut self, key: K, update: U) {
        self.backfill.entry(key).or_default().push(update);
    }

    /// Buffer a liquidity update from the pump (live WS) phase.
    pub fn buffer_pump(&mut self, key: K, update: U) {
        self.pump.entry(key).or_default().push(update);
    }

    /// Drain and return all backfill events for a pool key.
    ///
    /// Returns `None` if no backfill events exist for this key.
    /// The caller is responsible for applying the returned updates
    /// to the pool's tick data.
    pub fn drain_backfill(&mut self, key: &K) -> Option<Vec<U>> {
        self.backfill.remove(key)
    }

    /// Drain and return all pump events for a pool key.
    ///
    /// Returns `None` if no pump events exist for this key.
    /// The caller is responsible for applying the returned updates
    /// to the pool's tick data.
    pub fn drain_pump(&mut self, key: &K) -> Option<Vec<U>> {
        self.pump.remove(key)
    }

    /// Set the maximum age (in blocks) for pump buffer events.
    ///
    /// `None` means unbounded (no automatic expiry).
    pub const fn set_max_age(&mut self, max_age: Option<u64>) {
        self.max_age = max_age;
    }

    /// Return the total number of buffered events for a pool key
    /// (both backfill and pump).
    pub fn event_count(&self, key: &K) -> usize {
        let backfill = self.backfill.get(key).map_or(0, Vec::len);
        let pump = self.pump.get(key).map_or(0, Vec::len);
        backfill + pump
    }

    /// Discard all buffered events for all pools.
    ///
    /// Frees memory. Called when the operator knows that certain pools
    /// will never be registered.
    pub fn flush(&mut self) {
        self.backfill.clear();
        self.pump.clear();
    }

    /// Discard all buffered events (backfill + pump) for a single key.
    ///
    /// ADR-007 U3: the per-key inverse of `flush()`. Called by
    /// `BotState::unregister_pool` when removing a pool — a re-register must
    /// not replay stale buffered Mint/Burn/ModifyLiquidity onto the fresh
    /// pool. Silent no-op if the key was never buffered (matches `drain_*`).
    pub fn discard_for(&mut self, key: &K) {
        self.backfill.remove(key);
        self.pump.remove(key);
    }

    /// Expire pump buffer events whose `block_number` is older than
    /// `current_block - max_age`.
    ///
    /// If `max_age` is `None`, this is a no-op.
    /// Backfill buffer is never expired.
    pub fn expire(&mut self, current_block: u64)
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
pub trait LiquidityEvent {
    /// The block number at which this event occurred.
    fn block_number(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_for_clears_backfill_and_pump_for_one_key() {
        let mut buf: LiquidityEventBuffer<u32, u64> = LiquidityEventBuffer::new();

        // Two keys, each with a backfill + a pump event.
        buf.buffer_backfill(1, 100);
        buf.buffer_pump(1, 101);
        buf.buffer_backfill(2, 200);
        buf.buffer_pump(2, 201);

        // precondition: both keys hold 2 events
        assert_eq!(buf.event_count(&1), 2);
        assert_eq!(buf.event_count(&2), 2);

        // Discard only key 1.
        buf.discard_for(&1);

        // Key 1 is gone from both buffers; key 2 is untouched.
        assert_eq!(buf.event_count(&1), 0, "discard_for must clear key 1");
        assert_eq!(buf.event_count(&2), 2, "key 2 must be untouched");
    }

    #[test]
    fn discard_for_on_unknown_key_is_a_silent_no_op() {
        let mut buf: LiquidityEventBuffer<u32, u64> = LiquidityEventBuffer::new();
        buf.buffer_backfill(1, 100);

        // Discard a key that was never buffered.
        buf.discard_for(&99);

        // Existing key untouched.
        assert_eq!(buf.event_count(&1), 1);
    }
}
