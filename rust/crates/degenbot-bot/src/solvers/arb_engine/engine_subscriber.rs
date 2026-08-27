//! `PoolStateSubscriber` adapter wrapping a shared `ArbitrageEngine` (ADR-006 D4).
//!
//! The engine is shared as `Arc<Mutex<ArbitrageEngine>>` (the pump + Python both
//! hold clones). `EngineSubscriber` upgrades that to a subscriber: when
//! `Bot`'s `LogDispatcher` notifies `on_pool_state_updated(pool_id)`, this
//! adapter reads the shared `BotState` directly (no engine lock) and
//! classifies the pool into the shared `DirtySets` (no engine lock).
//!
//! RAYPAR engine-shard T3 (C42WKO): previously this adapter took the engine
//! `Mutex` to call `engine.insert_dirty`, parking behind every drain.
//! Now it holds a strong `Arc<DirtySets>` + `Arc<StateLock<BotState>>`
//! and writes the dirty marker under a short per-set lock — zero engine
//! contention with the drain path.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::bot_core::log_dispatcher::PoolStateSubscriber;
use crate::bot_core::state_lock::StateLock;
use crate::bot_core::BotState;
use crate::solvers::arb_engine::dirty_sets::DirtySets;
use crate::solvers::arb_engine::ArbitrageEngine;

/// A `PoolStateSubscriber` backed by a shared `ArbitrageEngine`.
///
/// Constructed from a `Weak<Mutex<ArbitrageEngine>>` so a de-registered engine
/// (all strong handles dropped) is silently skipped by the dispatcher's
/// `Weak::upgrade` — no leak, no panic. `Bot.attach_engine` receives this as a
/// `Weak<dyn PoolStateSubscriber>`.
pub struct EngineSubscriber {
    engine: Weak<Mutex<ArbitrageEngine>>,
    /// Shared dirty sets — written without taking the engine lock.
    dirty: Arc<DirtySets>,
    /// Shared core state for pool classification (V2/V3/V4).
    core: Arc<StateLock<BotState>>,
}

impl EngineSubscriber {
    /// Construct from a weak reference to the shared engine + the shared
    /// dirty sets + core state.
    #[must_use]
    pub(crate) fn new(
        engine: Weak<Mutex<ArbitrageEngine>>,
        dirty: Arc<DirtySets>,
        core: Arc<StateLock<BotState>>,
    ) -> Self {
        Self {
            engine,
            dirty,
            core,
        }
    }
}

impl PoolStateSubscriber for EngineSubscriber {
    fn on_pool_state_updated(&self, pool_id: u64) {
        // Liveness check: if the engine is gone, don't dirty (the drain won't
        // run to consume it). This upgrade does NOT lock the engine Mutex —
        // it just checks the Arc strong count.
        let Some(_engine) = self.engine.upgrade() else {
            return;
        };
        // Read core directly (no engine lock) to classify the pool.
        let core = self.core.read();
        if core.get_v2_pool_state(pool_id).is_some() {
            drop(core);
            self.dirty
                .insert(pool_id, degenbot_solvers::mixed::HopType::V2);
        } else if core.get_v3_pool(pool_id).is_some() {
            drop(core);
            self.dirty
                .insert(pool_id, degenbot_solvers::mixed::HopType::V3);
        } else if core.get_v4_pool(pool_id).is_some() {
            drop(core);
            self.dirty
                .insert(pool_id, degenbot_solvers::mixed::HopType::V4);
        }
        // Unregistered pool_id → no-op (no path references it).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solvers::arb_engine::ArbitrageEngine;
    use std::sync::Arc;

    #[test]
    fn adapter_forwards_notify_to_engine_insert_dirty() {
        let engine = Arc::new(Mutex::new(ArbitrageEngine::new()));
        let core = Arc::clone(engine.lock().core());
        let dirty = Arc::clone(&engine.lock().dirty_sets);
        let subscriber = EngineSubscriber::new(Arc::downgrade(&engine), dirty, core);

        // Engine has no pools registered → insert_dirty is a no-op, but the
        // adapter must upgrade + read core + call without panic.
        subscriber.on_pool_state_updated(42);

        // Dirty sets remain empty (pool 42 isn't registered).
        let engine_guard = engine.lock();
        assert!(
            engine_guard.dirty_sets_is_empty(),
            "unregistered pool must not dirty any set"
        );
    }

    /// A dead weak (engine dropped) → `on_pool_state_updated` silently no-ops.
    #[test]
    fn adapter_silently_skips_dropped_engine() {
        let engine = Arc::new(Mutex::new(ArbitrageEngine::new()));
        let core = Arc::clone(engine.lock().core());
        let dirty = Arc::clone(&engine.lock().dirty_sets);
        let subscriber = EngineSubscriber::new(Arc::downgrade(&engine), dirty, core);
        // Intentionally drop the engine AFTER constructing the subscriber.
        drop(engine);
        // Must not panic.
        subscriber.on_pool_state_updated(42);
    }
}
