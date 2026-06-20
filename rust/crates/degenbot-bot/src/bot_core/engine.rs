//! `Engine` — the Bot→Engine drain-side trait seam (ADR-006 D4, slice 6).
//!
//! The trait object solves the reference problem ADR-006 names ("Bot → Engine:
//! only a `Box<dyn EventSink>` — no strong type-bound knowledge the sink is
//! `UniswapArbEngine`"). [`SolveCoordinator`](super::solve_coordinator::SolveCoordinator)
//! holds `Vec<Arc<dyn Engine>>`; the concrete type (`UniswapEngine` via
//! `EngineHandle`) is known only at the wiring site (`py_binding.rs`).
//!
//! This is the leverage ADR-006 identifies: a future `AaveLiquidationEngine`
//! impls the same trait with zero `Bot`/pump/`SolveCoordinator` change. The
//! *divergence machinery* needed for a mid-flight-joining engine (per-engine
//! backfill queues, scoped dispatch) is deferred — see
//! `solve_coordinator.rs` docs for the preconditions that make this safe.
//!
//! All methods take `&self`: the engine `Mutex` lives *inside* the trait
//! object (the `EngineHandle` wrapper owns `Mutex<UniswapEngine>` and locks
//! per-call), so the coordinator never names `Mutex` directly. Lock order
//! `drain_lock` → `engine-Mutex` → `BotState` `RwLock` is preserved by the
//! wrapper locking only the engine (the `BotState` write was released by
//! `dispatch` before notify fired — slice 4's `EngineSubscriber` invariant).

use crate::bot_core::BlockMetadata;

/// The drain-side engine seam: the 6 methods [`DrainSink`](super::drain_sink::DrainSink)
/// fans out across engines (ADR-006 D4).
///
/// Conceptually the decomposition of the ADR's one-method `EventSink`
/// (`on_block`) into per-state-subject publish (slice 4's `LogDispatcher`
/// plus `PoolStateSubscriber`) plus this drain-point fan-out. The trait is
/// `Send + Sync` so `Arc<dyn Engine>` can cross the pump's tokio task.
#[allow(dead_code)]
pub trait Engine: Send + Sync {
    /// Solve every dirty path at `block` (the eager drain tick). Each engine
    /// owns its own dirty-set (seeded by `PoolStateSubscriber` notifications
    /// from `LogDispatcher`), so the coordinator does no dirty collection.
    fn solve_dirty(&self, block: u64, metadata: &BlockMetadata);

    /// Flush a debounced result batch to Python (the `DEBOUNCE_MS`
    /// send-debounce, owned by the pump — unchanged).
    fn send_result_batch(&self, metadata: &BlockMetadata);

    /// Are there unsolved dirty pool keys accumulated since the last drain?
    fn has_dirty_paths(&self) -> bool;

    /// Solve + advance at a genuine block boundary: solve any dirty paths
    /// carried over from the previous block, emit a block-boundary batch.
    /// Pumps the `last_solved_block` / `has_logs_this_block` bookkeeping
    /// locals owned by the pump.
    fn finalize_block(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    );

    /// The last block this engine solved. Used by `SolveCoordinator::start`
    /// to assert precondition 2 (cursors agree across engines before start).
    /// In steady state, callers should prefer the coordinator's
    /// `last_processed_block` (drain-consistent) over a per-engine read.
    fn last_processed_block(&self) -> Option<u64>;
}
