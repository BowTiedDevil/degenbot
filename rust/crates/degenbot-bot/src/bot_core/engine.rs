//! `Engine` — the Bot→Engine drain-side trait seam (ADR-006 D4, slice 6).
//!
//! The trait object solves the reference problem ADR-006 names ("Bot → Engine:
//! only a `Box<dyn EventSink>` — no strong type-bound knowledge the sink is
//! `ArbitrageEngine`"). [`SolveCoordinator`](super::solve_coordinator::SolveCoordinator)
//! holds `Vec<Arc<dyn Engine>>`; the concrete type (`ArbitrageEngine` via
//! `EngineHandle`) is known only at the wiring site (`py_binding.rs`).
//!
//! This is the leverage ADR-006 identifies: a future `AaveLiquidationEngine`
//! impls the same trait with zero `Bot`/pump/`SolveCoordinator` change. The
//! *divergence machinery* needed for a mid-flight-joining engine (per-engine
//! backfill queues, scoped dispatch) is deferred — see
//! `solve_coordinator.rs` docs for the preconditions that make this safe.
//!
//! All methods take `&self`: the engine `Mutex` lives *inside* the trait
//! object (the `EngineHandle` wrapper owns `Mutex<ArbitrageEngine>` and locks
//! per-call), so the coordinator never names `Mutex` directly. Lock order
//! `drain_lock` → `engine-Mutex` → `BotState` `RwLock` is preserved by the
//! wrapper locking only the engine (the `BotState` write was released by
//! `dispatch` before notify fired — slice 4's `EngineSubscriber` invariant).

use crate::bot_core::BlockMetadata;
use degenbot_solvers::mixed::MixedPoolRef;

/// The drain-side engine seam: the 6 methods [`DrainSink`](super::drain_sink::DrainSink)
/// fans out across engines (ADR-006 D4).
///
/// Conceptually the decomposition of the ADR's one-method `EventSink`
/// (`on_block`) into per-state-subject publish (slice 4's `LogDispatcher`
/// plus `PoolStateSubscriber`) plus this drain-point fan-out. The trait is
/// `Send + Sync` so `Arc<dyn Engine>` can cross the pump's tokio task.
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
    /// The `last_solved_block` + `has_logs_this_block` bookkeeping is owned
    /// by the engine itself since ergo task LEZJAS (the pump's `&mut` out-
    /// params retired); a mid-flight engine joining the pump can seed the
    /// last solved block via `set_last_solved_block` (ADR-006 D4).
    fn finalize_block(&self, block: u64, metadata: &BlockMetadata);

    /// Mark `block` as solved (engine-owned bookkeeping since ergo task
    /// LEZJAS). See `DrainSink::set_last_solved_block`.
    fn set_last_solved_block(&self, block: u64);

    /// Seed the cold-start `results_block` anchor to a settled block (the pump
    /// passes its resume/backfill boundary at resume so registration eager-solve
    /// candidates deliver at a valid, verification-safe solve block instead of
    /// block 0 or a deferred deferral). Only fills while `results_block` is 0.
    fn set_solve_anchor(&self, block: u64);

    /// Record a forward-log applied this block (engine-owned since LEZJAS).
    fn record_logs_this_block(&self);

    /// The last block this engine solved. Used by `SolveCoordinator::start`
    /// to assert precondition 2 (cursors agree across engines before start).
    /// In steady state, callers should prefer the coordinator's
    /// `last_processed_block` (drain-consistent) over a per-engine read.
    fn last_processed_block(&self) -> Option<u64>;

    /// Forward a `newHeads` block tick to the engine's block-notification
    /// channel (epic 6W35AI). The pump calls this on every
    /// `WsEvent::BlockHeader` it accepts, after advancing `current_block` —
    /// independent of solve/debounce state. MUST NOT touch `result_tx`
    /// (the result batch stays the solver's concern; the block channel is the
    /// authoritative block clock, not `ResultBatch::solve_block`).
    fn notify_block(&self, block: u64, metadata: &BlockMetadata);

    /// Snapshot every registered path's per-hop pool refs (the Option-A
    /// solver-state accuracy gate — see `solver_state_verifier`). Engines
    /// whose paths are not scalar-diffable (Solidly/Balancer/Curve) return
    /// empty; the arbitrage engine overrides with its `path_pools`.
    fn solver_path_pool_refs(&self) -> Vec<Vec<MixedPoolRef>> {
        Vec::new()
    }

    /// Consume-and-clear the ADR-021 solver-state change set (paths re-solved
    /// since the last publish). Defaults to empty; the arbitrage engine
    /// overrides with its accumulated `last_solved_path_ids`. The caller (pump
    /// publish point) hands this to the solver-state verifier so it diffs only
    /// this block's re-solved paths against the chain — never the whole
    /// registered set.
    fn take_solver_path_pool_refs_change_set(&self) -> Vec<Vec<MixedPoolRef>> {
        Vec::new()
    }
}
