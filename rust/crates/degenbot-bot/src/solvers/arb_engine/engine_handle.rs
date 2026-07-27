//! `EngineHandle` — the `Arc<dyn Engine>` wrapper for a shared
//! `ArbitrageEngine` (ADR-006 D4, slice 6).
//!
//! [`SolveCoordinator`](crate::bot_core::solve_coordinator::SolveCoordinator)
//! holds `Arc<dyn Engine>` and never names `Mutex` or `ArbitrageEngine`
//! directly. This wrapper holds a strong clone of the shared
//! `Arc<parking_lot::Mutex<ArbitrageEngine>>` (the same `Arc` that
//! `PyArbitrageEngine.engine` and the pump's strong handle both reference),
//! and implements [`Engine`](crate::bot_core::engine::Engine) by
//! locking-then-forwarding — so the engine's lock is encapsulated behind the
//! trait object.
//!
//! Replaces slice 5a's `EngineDrainSink` (a `Weak<Mutex<ArbitrageEngine>>`
//! pass-through that held the concrete type leakily and impl'd `DrainSink`).
//! The handle is the strong-owning, trait-erased counterpart: the coordinator
//! sees only `Arc<dyn Engine>`.
//!
//! **Lock order (D2):** each method locks the engine `Mutex` **alone** —
//! the `BotState` write guard from the preceding `Bot::dispatch_log` is
//! already released (slice 4's `EngineSubscriber` invariant), and the engine
//! takes its own core `read`/`write` internally. Coordinator `drain_lock` →
//! `engine-Mutex` → `BotState` `RwLock`, never reversed.

use std::sync::{Arc, Weak};

use crate::bot_core::engine::Engine;
use crate::bot_core::log_dispatcher::PoolStateSubscriber;
use crate::bot_core::BlockMetadata;

use super::engine_subscriber::EngineSubscriber;
use super::ArbitrageEngine;

/// A `Arc<dyn Engine>` view over a shared `Arc<parking_lot::Mutex<ArbitrageEngine>>`.
///
/// The strong handle the wiring layer (`py_binding.rs`) clones into the
/// coordinator's engine vector. `PyArbitrageEngine.engine` retains its own
/// strong clone for direct engine access (e.g. `register_path`,
/// `latest_results`) not on the `Engine` drain trait; both clones reference
/// the same underlying `ArbitrageEngine`.
pub struct EngineHandle {
    engine: Arc<parking_lot::Mutex<ArbitrageEngine>>,
    /// Strong owner of the `EngineSubscriber` (ADR-006 cycle-free topology:
    /// the strong lives on the engine side, co-owned with `engine`).
    /// `LogDispatcher::notify` holds only a `Weak<dyn PoolStateSubscriber>`;
    /// its `upgrade()` succeeds until every `EngineHandle` (and thus the
    /// engine) drops — the subscriber dies with the engine, no leak, no cycle.
    ///
    /// This field exists because `EngineSubscriber::weak_handle` returned a
    /// dangling `Weak` (its strong dropped on return). Holding the strong here
    /// is the lift: the subscriber is alive for the engine's lifetime.
    subscriber: Arc<dyn PoolStateSubscriber>,
}

impl EngineHandle {
    /// Construct from a strong clone of the shared engine handle.
    ///
    /// Builds and holds the strong `EngineSubscriber` (the cycle-free home
    /// for the dispatcher's `Weak`) — see [`subscriber_weak`](Self::subscriber_weak).
    #[must_use]
    pub fn new(engine: Arc<parking_lot::Mutex<ArbitrageEngine>>) -> Self {
        let subscriber: Arc<dyn PoolStateSubscriber> =
            Arc::new(EngineSubscriber::new(Arc::downgrade(&engine)));
        Self { engine, subscriber }
    }

    /// A `Weak<dyn PoolStateSubscriber>` that stays live while this `EngineHandle`
    /// (and thus the engine) lives. The wiring layer hands this to
    /// `Bot::attach_engine` at `register_path` time so `LogDispatcher::notify`
    /// routes `on_pool_state_updated` → `insert_dirty` on the live engine.
    ///
    /// Replaces the deleted `EngineSubscriber::weak_handle`, which returned a
    /// dangling `Weak` (its strong dropped on return — hotpath capture
    /// 2026-07-14 showed 71 notifies → 0 dirties because every `upgrade()`
    /// returned `None`).
    #[must_use]
    pub fn subscriber_weak(&self) -> Weak<dyn PoolStateSubscriber> {
        Arc::downgrade(&self.subscriber)
    }

    /// Construct an `Arc<dyn Engine>` from a strong clone — the convenience
    /// the wiring layer uses to populate `SolveCoordinator::new`.
    #[must_use]
    pub fn arc_dyn(engine: Arc<parking_lot::Mutex<ArbitrageEngine>>) -> Arc<dyn Engine> {
        Arc::new(Self::new(engine))
    }
}

impl Engine for EngineHandle {
    /// Hold-time invariant (ergo 3HYYGQ, assessed 2026-06 — no refactor).
    ///
    /// This call holds the engine `Mutex` for the whole `solve_dirty`,
    /// including the rayon `par_iter` solve in `rebuild_and_solve_affected`
    /// (~5-20ms for 100-200 affected paths). This is **acceptable**: Python's
    /// result hot path consumes batches via the unbounded `mpsc` channel
    /// (`PyArbitrageEngine::__anext__` → `rx.recv().await`), which never
    /// acquires the engine lock, so the hold does NOT block result delivery.
    /// The only cross-thread contenders during a solve are:
    ///   - `EngineSubscriber::insert_dirty` (queues a dirty marker for the
    ///     NEXT solve — a delayed mark is absorbed by the following
    ///     `solve_dirty` iteration, so the delay is benign);
    ///   - non-hot-path introspection/admin methods (`inspect_path`,
    ///     `latest_results`, `deregister_path`, `set_profit_thresholds`).
    ///
    /// Do NOT speculatively split this lock (e.g. release around the rayon
    /// `par_iter`) without first re-deriving interleaving safety for
    /// `register_path`/`deregister_path`: those run GIL-released on the
    /// Python thread concurrently with the pump and currently rely on this
    /// single hold for serialization. `latest_results()` is test/admin-only —
    /// grep-verified absent from the example hot loop, which is
    /// `async for batch in engine_registry.engine:`.
    #[hotpath::measure(label = "EngineHandle::solve_dirty")]
    fn solve_dirty(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().solve_dirty(block, metadata);
    }

    #[hotpath::measure(label = "EngineHandle::send_result_batch")]
    fn send_result_batch(&self, metadata: &BlockMetadata) {
        self.engine.lock().send_result_batch(metadata);
    }

    #[hotpath::measure(label = "EngineHandle::has_dirty_paths")]
    fn has_dirty_paths(&self) -> bool {
        self.engine.lock().has_dirty_paths()
    }

    #[hotpath::measure(label = "EngineHandle::finalize_block")]
    fn finalize_block(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().finalize_block(block, metadata);
    }

    fn set_last_solved_block(&self, block: u64) {
        self.engine.lock().set_last_solved_block(block);
    }

    fn record_logs_this_block(&self) {
        self.engine.lock().record_logs_this_block();
    }

    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    #[hotpath::measure(label = "EngineHandle::notify_block")]
    fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().notify_block(block, metadata);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED→GREEN tracer (slice 6): `EngineHandle` forwards each `Engine`
    /// method to the underlying `ArbitrageEngine` without panic. Empty engine
    /// → no dirty paths; `solve_dirty`/`send_result_batch` are no-ops but
    /// must not panic.
    #[test]
    fn engine_handle_forwards_calls_without_panic() {
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        let handle = EngineHandle::new(engine);
        let metadata = BlockMetadata::default();

        assert!(!handle.has_dirty_paths(), "fresh engine has no dirty paths");
        assert_eq!(
            handle.last_processed_block(),
            None,
            "fresh engine processed no block"
        );

        handle.solve_dirty(1, &metadata);
        handle.send_result_batch(&metadata);
        handle.set_last_solved_block(0);
        handle.finalize_block(1, &metadata);

        assert!(!handle.has_dirty_paths());
    }

    /// GREEN (fix for the hotpath-captured bug 2026-07-14): `subscriber_weak`
    /// returns a `Weak<dyn PoolStateSubscriber>` whose `upgrade()` succeeds
    /// while the `EngineHandle` (and thus the engine) lives. This is the
    /// invariant `LogDispatcher::notify` relies on to route
    /// `on_pool_state_updated` → `insert_dirty`.
    ///
    /// The deleted `EngineSubscriber::weak_handle` returned a *dangling* Weak
    /// (its strong dropped on return) — the 5-minute mainnet capture showed
    /// 71 WS logs reaching `notify` but 0 `on_drain`/`solve_dirty`, because
    /// every `weak.upgrade()` returned `None`. Holding the strong on
    /// `EngineHandle` (the ADR-006 cycle-free engine-side owner) is the lift.
    #[test]
    fn subscriber_weak_stays_live_while_engine_handle_lives() {
        let weak = {
            let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
            let handle = EngineHandle::new(engine);
            handle.subscriber_weak() // handle drops at end of block
        };
        // EngineHandle dropped → the strong subscriber dropped → Weak is dead.
        assert!(
            weak.upgrade().is_none(),
            "subscriber Weak should dangle once EngineHandle drops"
        );

        // And the positive case: held alive → upgrade succeeds.
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        let handle = EngineHandle::new(engine);
        let weak = handle.subscriber_weak();
        assert!(
            weak.upgrade().is_some(),
            "subscriber Weak must upgrade while EngineHandle is alive — \
             LogDispatcher::notify depends on this to fire on_pool_state_updated"
        );
    }
}
