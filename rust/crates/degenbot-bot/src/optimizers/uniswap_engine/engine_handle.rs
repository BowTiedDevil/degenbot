//! `EngineHandle` — the `Arc<dyn Engine>` wrapper for a shared
//! `UniswapEngine` (ADR-006 D4, slice 6).
//!
//! [`SolveCoordinator`](crate::bot_core::solve_coordinator::SolveCoordinator)
//! holds `Arc<dyn Engine>` and never names `Mutex` or `UniswapEngine`
//! directly. This wrapper holds a strong clone of the shared
//! `Arc<parking_lot::Mutex<UniswapEngine>>` (the same `Arc` that
//! `PyUniswapArbEngine.engine` and the pump's strong handle both reference),
//! and implements [`Engine`](crate::bot_core::engine::Engine) by
//! locking-then-forwarding — so the engine's lock is encapsulated behind the
//! trait object.
//!
//! Replaces slice 5a's `EngineDrainSink` (a `Weak<Mutex<UniswapEngine>>`
//! pass-through that held the concrete type leakily and impl'd `DrainSink`).
//! The handle is the strong-owning, trait-erased counterpart: the coordinator
//! sees only `Arc<dyn Engine>`.
//!
//! **Lock order (D2):** each method locks the engine `Mutex` **alone** —
//! the `BotState` write guard from the preceding `Bot::dispatch_log` is
//! already released (slice 4's `EngineSubscriber` invariant), and the engine
//! takes its own core `read`/`write` internally. Coordinator `drain_lock` →
//! `engine-Mutex` → `BotState` `RwLock`, never reversed.

use std::sync::Arc;

use crate::bot_core::engine::Engine;
use crate::bot_core::BlockMetadata;

use super::UniswapEngine;

/// A `Arc<dyn Engine>` view over a shared `Arc<parking_lot::Mutex<UniswapEngine>>`.
///
/// The strong handle the wiring layer (`py_binding.rs`) clones into the
/// coordinator's engine vector. `PyUniswapArbEngine.engine` retains its own
/// strong clone for direct engine access (e.g. `register_path`,
/// `latest_results`) not on the `Engine` drain trait; both clones reference
/// the same underlying `UniswapEngine`.
pub struct EngineHandle {
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
}

impl EngineHandle {
    /// Construct from a strong clone of the shared engine handle.
    #[must_use]
    pub fn new(engine: Arc<parking_lot::Mutex<UniswapEngine>>) -> Self {
        Self { engine }
    }

    /// Construct an `Arc<dyn Engine>` from a strong clone — the convenience
    /// the wiring layer uses to populate `SolveCoordinator::new`.
    #[must_use]
    pub fn arc_dyn(engine: Arc<parking_lot::Mutex<UniswapEngine>>) -> Arc<dyn Engine> {
        Arc::new(Self::new(engine))
    }
}

impl Engine for EngineHandle {
    fn solve_dirty(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().solve_dirty(block, metadata);
    }

    fn send_result_batch(&self, metadata: &BlockMetadata) {
        self.engine.lock().send_result_batch(metadata);
    }

    fn has_dirty_paths(&self) -> bool {
        self.engine.lock().has_dirty_paths()
    }

    fn finalize_block(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        self.engine
            .lock()
            .finalize_block(block, metadata, last_solved_block, has_logs_this_block);
    }

    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED→GREEN tracer (slice 6): `EngineHandle` forwards each `Engine`
    /// method to the underlying `UniswapEngine` without panic. Empty engine
    /// → no dirty paths; `solve_dirty`/`send_result_batch` are no-ops but
    /// must not panic.
    #[test]
    fn engine_handle_forwards_calls_without_panic() {
        let engine = Arc::new(parking_lot::Mutex::new(UniswapEngine::new()));
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
        let mut last_solved = 0u64;
        let mut has_logs = false;
        handle.finalize_block(1, &metadata, &mut last_solved, &mut has_logs);

        assert!(!handle.has_dirty_paths());
    }
}
