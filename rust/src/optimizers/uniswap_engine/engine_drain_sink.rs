//! `DrainSink` adapter wrapping a shared `UniswapEngine` (ADR-006 D4, slice 5).
//!
//! The placeholder [`DrainSink`](crate::bot_core::drain_sink::DrainSink) impl:
//! a faithful pass-through to `UniswapEngine` so the bot keeps solving while
//! `SolveCoordinator` (slice 6) + `ReorgCoordinator` (slice 7) are built. Each
//! method locks the engine `Mutex` ALONE (the `BotState` write guard from the
//! preceding `bot.dispatch_log` is already released) and forwards.
//!
//! `BlockPump` holds this as `Arc<dyn DrainSink>`; the wiring layer
//! (`PyBot`/`PyUniswapArbEngine`) constructs it from the shared engine handle.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::bot_core::drain_sink::DrainSink;
use crate::optimizers::uniswap_engine::{BlockMetadata, UniswapEngine};

/// A `DrainSink` backed by a shared `UniswapEngine` (slice 5 placeholder).
///
/// Constructed from a `Weak<Mutex<UniswapEngine>>` so a de-registered engine
/// (all strong handles dropped) fails gracefully — though in practice the pump
/// holds a strong handle for its lifetime, so upgrade always succeeds.
#[allow(dead_code)]
pub(crate) struct EngineDrainSink {
    engine: Weak<Mutex<UniswapEngine>>,
}

impl EngineDrainSink {
    /// Construct from a weak reference to the shared engine.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new(engine: Weak<Mutex<UniswapEngine>>) -> Self {
        Self { engine }
    }

    /// Construct an `Arc<dyn DrainSink>` from a strong engine handle — the
    /// convenience the wiring layer uses.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn arc_handle(engine: &Arc<Mutex<UniswapEngine>>) -> Arc<dyn DrainSink> {
        Arc::new(Self {
            engine: Arc::downgrade(engine),
        })
    }

    /// Upgrade the weak ref, panicking if the engine was dropped mid-drain
    /// (the pump holds a strong handle, so this is a wiring bug, not a race).
    fn engine(&self) -> Arc<Mutex<UniswapEngine>> {
        self.engine
            .upgrade()
            .expect("EngineDrainSink: engine dropped while pump is live — wiring bug")
    }
}

impl DrainSink for EngineDrainSink {
    fn has_dirty_paths(&self) -> bool {
        self.engine().lock().has_dirty_paths()
    }

    fn on_drain(&self, block: u64, metadata: &BlockMetadata) {
        self.engine().lock().solve_dirty(block, metadata);
    }

    fn on_send(&self, metadata: &BlockMetadata) {
        self.engine().lock().send_result_batch(metadata);
    }

    fn finalize_block(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        self.engine().lock().finalize_block(
            block,
            metadata,
            last_solved_block,
            has_logs_this_block,
        );
    }

    fn on_reorg(&self, block: u64) {
        self.engine().lock().handle_reorg(block);
    }

    fn last_processed_block(&self) -> Option<u64> {
        self.engine().lock().last_processed_block()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED→GREEN tracer (ADR-006 slice 5): `EngineDrainSink` forwards each
    /// drain-sink call to the engine without panic. With an unregistered
    /// engine (no pools, no paths), every op is a no-op but must not panic —
    /// and `has_dirty_paths` / `last_processed_block` reflect the empty state.
    #[test]
    fn engine_drain_sink_forwards_calls_without_panic() {
        let engine = Arc::new(Mutex::new(UniswapEngine::new()));
        let sink = EngineDrainSink::arc_handle(&engine);

        // Empty engine → no dirty paths, no processed block.
        assert!(!sink.has_dirty_paths(), "fresh engine has no dirty paths");
        assert_eq!(
            sink.last_processed_block(),
            None,
            "fresh engine has processed no block"
        );

        let metadata = BlockMetadata {
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };

        // Each op must forward without panicking on an empty engine.
        sink.on_drain(1, &metadata);
        sink.on_send(&metadata);
        let mut last_solved = 0u64;
        let mut has_logs = false;
        sink.finalize_block(1, &metadata, &mut last_solved, &mut has_logs);
        sink.on_reorg(1);

        // on_drain solved nothing → still no dirty paths.
        assert!(!sink.has_dirty_paths());
    }

    /// A dead weak (engine dropped mid-test) panics loudly — a wiring bug,
    /// never a silent no-op (the pump holds a strong handle, so this only
    /// happens if the wiring is broken).
    #[test]
    #[should_panic(expected = "EngineDrainSink: engine dropped while pump is live")]
    fn engine_drain_sink_panics_on_dropped_engine() {
        let sink = {
            let engine = Arc::new(Mutex::new(UniswapEngine::new()));
            EngineDrainSink::new(Arc::downgrade(&engine))
            // engine drops here → weak goes dead.
        };
        // has_dirty_paths upgrades → panics (wiring bug).
        let _ = sink.has_dirty_paths();
    }
}
