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

    /// The highest block number whose logs the pump has FULLY processed
    /// (every log for that block has been buffered). `drain_pump_completed`
    /// yields only events with `block_number <= last_complete_block`, leaving
    /// any partly-arrived same-block tail buffered — closing the rolling-
    /// start race where `apply_buffer` drains mid-block and pins a state that
    /// has `update_block == N` but is missing a later same-block log. Set by
    /// `mark_block_complete` from the pump's per-block finalize path.
    last_complete_block: Option<u64>,
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
            last_complete_block: None,
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
    ///
    /// **Race warning:** this drains EVERY buffered pump event regardless of
    /// whether the pump has finished processing the event's block. During a
    /// rolling start (`build_paths` registering a pool while the live pump is
    /// mid-block), this can drain a partial block — pinning a state with
    /// `update_block == N` that is missing a later same-block log. Prefer
    /// [`drain_pump_completed`](Self::drain_pump_completed) at the
    /// registration (drain+pin) seam.
    pub fn drain_pump(&mut self, key: &K) -> Option<Vec<U>> {
        self.pump.remove(key)
    }

    /// Mark `block` as fully processed by the pump — every log for `block`
    /// has been buffered. Called from the pump's per-block finalize path.
    /// Monotonic: a lower block than the current marker is ignored (the pump
    /// may re-report an already-completed block on a racing re-drain).
    pub fn mark_block_complete(&mut self, block: u64) {
        self.last_complete_block =
            Some(std::cmp::max(self.last_complete_block.unwrap_or(0), block));
    }

    /// Drain and return the pump events for `key` whose `block_number` is at
    /// or below [`mark_block_complete`](Self::mark_block_complete)'s highest
    /// fully-processed block. Events for a block the pump has NOT finished
    /// processing stay buffered, so a drain+pin at the registration seam
    /// cannot capture a half-delivered block.
    ///
    /// Returns `None` if no completable pump events exist for this key (no
    /// events at all, or `mark_block_complete` was never called, or all
    /// buffered events are for a still-in-progress block). Events left below
    /// the completeness gate are retained for a subsequent drain.
    pub fn drain_pump_completed(&mut self, key: &K) -> Option<Vec<U>>
    where
        U: LiquidityEvent,
    {
        let cutoff = self.last_complete_block?;
        let events = self.pump.get_mut(key)?;
        // Partition: completable (block_number <= cutoff) stays; the in-progress
        // tail (block_number > cutoff) is retained for the next drain.
        let (completable, tail): (Vec<_>, Vec<_>) = std::mem::take(events)
            .into_iter()
            .partition(|e| e.block_number() <= cutoff);
        if !tail.is_empty() {
            *events = tail;
        }
        if completable.is_empty() {
            None
        } else {
            Some(completable)
        }
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

pub use crate::liquidity_event::LiquidityEvent;

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

    // ── drain_pump_completed block-completeness gate (YLYJM2 race fix) ────
    //
    // The rolling-start race: `apply_buffer` drains the pump buffer mid-block
    // and pins `(tick_data, update_block=N)`, but the pump hadn't yet
    // buffered a LATER same-block log (logIdx 1433 > 120). That log then lands
    // AFTER the pin, orphaned, and the verify reads on-chain@N (with both
    // logs) vs the pinned pair (only the first log) → mismatch.

    /// A test event with an explicit block number + a tag to distinguish
    /// same-block events by log index.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Ev {
        block: u64,
        tag: u32,
    }
    impl LiquidityEvent for Ev {
        fn block_number(&self) -> u64 {
            self.block
        }
    }

    /// The block-completeness gate: two same-block events, only the first's
    /// block is marked complete. The drain yields the first block's events and
    /// RETAINS the in-progress block's events.
    #[test]
    fn drain_pump_completed_yields_only_fully_processed_blocks() {
        let mut buf: LiquidityEventBuffer<u32, Ev> = LiquidityEventBuffer::new();
        // Block 100: two events, fully delivered.
        buf.buffer_pump(1, Ev { block: 100, tag: 1 });
        buf.buffer_pump(1, Ev { block: 100, tag: 2 });
        // Block 101: the in-progress block — the pump has buffered one of its
        // logs (logIdx 120) but not the later same-block log (logIdx 1433).
        buf.buffer_pump(1, Ev { block: 101, tag: 3 });
        buf.mark_block_complete(100);

        let drained = buf
            .drain_pump_completed(&1)
            .expect("block 100 events drain");
        assert_eq!(drained.len(), 2, "both events for the complete block 100");
        assert!(drained.iter().all(|e| e.block == 100));
        // The in-progress block 101 event stays buffered.
        assert_eq!(buf.event_count(&1), 1, "block 101 event retained");
    }

    /// Reproduces the exact failure shape at block 25642266: two same-block
    /// `ModifyLiquidity` events (logIdx 120 + 1433); the drain at logIdx 120's
    /// instant must NOT take the not-yet-arrived logIdx 1433's event, and must
    /// NOT advance `update_block` past the last complete block.
    #[test]
    fn drain_pump_completed_prevents_the_25642266_half_block_pin() {
        let mut buf: LiquidityEventBuffer<u32, Ev> = LiquidityEventBuffer::new();
        // The pump delivered the FIRST same-block log (logIdx 120) for block
        // 25642266, marked complete at 25642265 (the PREVIOUS block).
        buf.buffer_pump(
            1,
            Ev {
                block: 25_642_266,
                tag: 120,
            },
        );
        buf.mark_block_complete(25_642_265);
        // `apply_buffer` drains RIGHT NOW — before logIdx 1433 arrives.
        let drained = buf.drain_pump_completed(&1);
        assert!(
            drained.is_none(),
            "block 25642266 is NOT complete; nothing drains"
        );
        // ...later, logIdx 1433 arrives for the same block...
        buf.buffer_pump(
            1,
            Ev {
                block: 25_642_266,
                tag: 1433,
            },
        );
        // ...the pump finishes the block...
        buf.mark_block_complete(25_642_266);
        // ...now BOTH same-block events drain together, atomically.
        let drained = buf
            .drain_pump_completed(&1)
            .expect("both events after block completes");
        assert_eq!(drained.len(), 2, "both same-block events drain together");
        let mut tags: Vec<_> = drained.iter().map(|e| e.tag).collect();
        tags.sort_unstable();
        assert_eq!(
            tags,
            vec![120, 1433],
            "log-index order preserved within the block"
        );
    }

    /// `drain_pump_completed` with no `mark_block_complete` ever called drains
    /// nothing (defensive — the gate defaults closed, not open).
    #[test]
    fn drain_pump_completed_defaults_closed_with_no_marker() {
        let mut buf: LiquidityEventBuffer<u32, Ev> = LiquidityEventBuffer::new();
        buf.buffer_pump(1, Ev { block: 100, tag: 1 });
        assert!(
            buf.drain_pump_completed(&1).is_none(),
            "no complete block → nothing drains"
        );
        assert_eq!(buf.event_count(&1), 1, "event retained");
    }
}
