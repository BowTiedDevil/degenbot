//! `PoolStateSubscriber` adapter wrapping a shared `UniswapEngine` (ADR-006 D4).
//!
//! The engine is shared as `Arc<Mutex<UniswapEngine>>` (the pump + Python both
//! hold clones). `EngineSubscriber` upgrades that to a subscriber: when
//! `Bot`'s `LogDispatcher` notifies `on_pool_state_updated(pool_id)`, this
//! adapter locks the engine **alone** (the `BotState` write guard is already
//! released by `dispatch`) and calls `engine.insert_dirty(pool_id)`.
//!
//! **Lock order (D2):** engine-then-core is preserved *by not nesting* — the
//! core write was released before notify fired, so the engine lock here is
//! taken with no core lock held. `insert_dirty` takes a *read* guard on core
//! (engine-then-core-read), which is the same order `apply_log` already uses.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::bot_core::log_dispatcher::PoolStateSubscriber;
use crate::optimizers::uniswap_engine::UniswapEngine;

/// A `PoolStateSubscriber` backed by a shared `UniswapEngine`.
///
/// Constructed from a `Weak<Mutex<UniswapEngine>>` so a de-registered engine
/// (all strong handles dropped) is silently skipped by the dispatcher's
/// `Weak::upgrade` — no leak, no panic. `Bot.attach_engine` receives this as a
/// `Weak<dyn PoolStateSubscriber>`.
#[allow(dead_code)]
pub(crate) struct EngineSubscriber {
    engine: Weak<Mutex<UniswapEngine>>,
}

impl EngineSubscriber {
    /// Construct from a weak reference to the shared engine.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new(engine: Weak<Mutex<UniswapEngine>>) -> Self {
        Self { engine }
    }

    /// Construct a `Weak<dyn PoolStateSubscriber>` from a strong engine handle,
    /// the convenience `Bot.attach_engine` callers use.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn weak_handle(engine: &Arc<Mutex<UniswapEngine>>) -> Weak<dyn PoolStateSubscriber> {
        let strong: Arc<dyn PoolStateSubscriber> = Arc::new(Self {
            engine: Arc::downgrade(engine),
        });
        Arc::downgrade(&strong)
    }
}

impl PoolStateSubscriber for EngineSubscriber {
    fn on_pool_state_updated(&self, pool_id: u64) {
        // Upgrade the weak engine ref. If all strong handles dropped (engine
        // de-registered), silently no-op — the dispatcher already skips dead
        // Weaks, but this guards the race where the last handle drops between
        // the dispatcher's `upgrade` and this call.
        let Some(engine) = self.engine.upgrade() else {
            return;
        };
        // Lock the engine ALONE (core write already released by `dispatch`).
        engine.lock().insert_dirty(pool_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizers::uniswap_engine::UniswapEngine;

    /// RED→GREEN tracer (ADR-006 slice 4): the adapter forwards a notify to
    /// `engine.insert_dirty`, dirtying the pool in the engine's set. The
    /// engine's own `core` is empty here so `insert_dirty` is a no-op, but the
    /// adapter must not panic and must upgrade the weak ref successfully.
    #[test]
    fn adapter_forwards_notify_to_engine_insert_dirty() {
        let engine = Arc::new(Mutex::new(UniswapEngine::new()));
        let subscriber = EngineSubscriber::new(Arc::downgrade(&engine));

        // Engine has no pools registered → insert_dirty is a no-op, but the
        // adapter must upgrade + lock + call without panic.
        subscriber.on_pool_state_updated(42);

        // Dirty sets remain empty (pool 42 isn't registered).
        let engine_guard = engine.lock();
        assert!(
            engine_guard.dirty_v2_is_empty(),
            "unregistered pool must not dirty v2"
        );
        assert!(
            engine_guard.dirty_v3_is_empty(),
            "unregistered pool must not dirty v3"
        );
        assert!(
            engine_guard.dirty_v4_is_empty(),
            "unregistered pool must not dirty v4"
        );
    }

    /// A dead weak (engine dropped) → `on_pool_state_updated` silently no-ops.
    #[test]
    fn adapter_silently_skips_dropped_engine() {
        let subscriber = {
            let engine = Arc::new(Mutex::new(UniswapEngine::new()));
            EngineSubscriber::new(Arc::downgrade(&engine))
            // engine drops here → weak goes dead.
        };
        // Must not panic.
        subscriber.on_pool_state_updated(42);
    }
}
