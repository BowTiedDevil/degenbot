//! `DrainSink` — the pump's per-block-drain + reorg seam (ADR-006 D4).
//!
//! `BlockPump` (Bot's WS transport + drain loop) holds an `Arc<dyn DrainSink>`
//! instead of `Arc<Mutex<UniswapEngine>>`. Per WS log, the pump calls
//! `bot.dispatch_log(log)` (slice 4: decode→apply→notify, which dirties the
//! engine via the [`EngineSubscriber`](crate::optimizers::uniswap_engine::engine_subscriber) adapter).
//! At block boundaries / drain ticks / reorg, the pump calls the sink:
//! - `on_drain` solves the dirty paths the subscriber accumulated.
//! - `on_send` flushes a debounced result batch to Python.
//! - `finalize_block` solves+advances at a genuine block boundary.
//! - `on_reorg` restores state before the forked block.
//!
//! **Status (slice 6):** the live impl is
//! [`SolveCoordinator`](crate::bot_core::solve_coordinator::SolveCoordinator),
//! which fans drain-tick / send / finalize / reorg calls to every attached
//! [`Engine`](crate::bot_core::engine::Engine) under a `drain_lock` and
//! exposes a drain-consistent `last_processed_block` (Python polls block
//! until an in-flight drain completes — no Rust/Python race). The slice-5a
//! `EngineDrainSink` placeholder was deleted. `ReorgCoordinator` (slice 7)
//! adds optimistic per-event `restore_before_block`.
//!
//! **Lock order (D2):** `bot.dispatch_log` acquires the `BotState` write guard,
//! applies, RELEASES, then notifies the engine subscriber (engine `Mutex`
//! alone). The sink methods take the engine `Mutex` with NO core write held
//! (the dispatch already released it). `on_drain`/`on_send`/`finalize_block`
//! acquire core *write* guards internally — engine-then-core, never reversed.

use crate::bot_core::BlockMetadata;

/// The per-block drain + reorg seam the `BlockPump` drives (ADR-006 D4).
///
/// `apply_log` is deliberately ABSENT — log application routes through
/// `Bot::dispatch_log` (slice 4's `LogDispatcher` → `BotState` apply →
/// `EngineSubscriber` dirty-mark), so `process_block`/`process_block_and_send`
/// decompose in the pump into `dispatch_log`-per-log + `on_drain` (+
/// `on_send`). This routes ALL log application through the dispatcher (the D4
/// goal) — the sink only owns solve/send/reorg.
#[allow(dead_code)]
pub(crate) trait DrainSink: Send + Sync {
    /// Are there unsolved dirty pool keys accumulated since the last drain?
    #[must_use]
    fn has_dirty_paths(&self) -> bool;

    /// Solve every dirty path at `block` (the eager drain tick).
    fn on_drain(&self, block: u64, metadata: &BlockMetadata);

    /// Flush a debounced result batch to Python (the `DEBOUNCE_MS` send-debounce).
    fn on_send(&self, metadata: &BlockMetadata);

    /// Solve + advance at a genuine block boundary: solve any dirty paths
    /// carried over from the previous block and emit a block-boundary batch.
    /// Pumps the `last_solved_block` / `has_logs_this_block` bookkeeping locals.
    fn finalize_block(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    );

    /// A WS log arrived with `removed: true` — restore state to just before
    /// `block` (the orphaned block's predecessor). Idempotent.
    fn on_reorg(&self, block: u64);

    /// The last block the sink processed — the backfill-start boundary used by
    /// `Bot::start` / the pump's subscribe phase.
    #[must_use]
    fn last_processed_block(&self) -> Option<u64>;
}
