//! `PumpControl` — the driver-facing control seam (ADR-046).
//!
//! ADR-041's seam retirement landed the pump↔engine coordination on the ONE
//! `StageHandlers` trait, mixing two vocabularies: the eight pure stage hooks
//! (fact-carrying, product-shaped) and seven driver pokes (bookkeeping and
//! liveness). The candidate-2 deepening splits those layers:
//!
//! - **`StageHandlers`** stays the pure product/facts seam — exactly the
//!   eight `on_*` stage hooks, no control pokes.
//! - **`PumpControl`** (this module) owns the seven pokes the runtime drives
//!   between stage transitions. It is a *separate required trait*, injected
//!   beside `Arc<dyn StageHandlers>` at pump construction; both `EngineStages`
//!   and the two test doubles (`NoopStubEngine`, `FakeStageEngine`) implement
//!   it, so adding a poke without implementing it is a compile error (the same
//!   compile-fails-if-incomplete property ADR-041 pins for the hooks).
//!
//! The cursor coordinates are `Epoch` (the one block coordinate) — the
//! `u64`-typed twins that drifted in `EngineStages` are the evidence for the
//! split. `notify_block` stays raw `u64`: a `newHeads` tick is a chain fact
//! forwarded to the delivery-to-Python block clock, not engine epoch work.

use super::{BlockMetadata, Epoch};

/// The driver-facing control seam: the seven pokes the pump drives between
/// stage transitions (ADR-046). Implementors: `EngineStages` and the
/// test-declared conformance doubles.
pub trait PumpControl: Send + Sync + 'static {
    /// Are there unsolved dirty pool keys accumulated since the last solve?
    /// The driver's drained-settle gate (Streaming → Resolved readiness).
    #[must_use]
    fn has_dirty_paths(&self) -> bool;

    /// Mark `solved` as solved (engine-owned bookkeeping).
    fn set_last_solved_block(&self, solved: Epoch);

    /// Seed the cold-start `results_block` anchor to a settled block (the
    /// pump's resume/backfill boundary). Only fills while it is 0.
    fn set_solve_anchor(&self, anchor: Epoch);

    /// Record that at least one forward log applied this block (cleared by
    /// the next finalize).
    fn record_logs_this_block(&self);

    /// The last block this engine solved. The resume path reads it to seed
    /// the machine's starting cursor.
    #[must_use]
    fn last_processed_block(&self) -> Option<Epoch>;

    /// Forward a `newHeads` tick to the delivery-to-Python block clock
    /// (the one non-FIFO dispatch: a chain fact never queues behind solver
    /// work; every accepted header delivered 1:1). Raw `u64` by design —
    /// a chain coordinate, not an engine epoch.
    fn notify_block(&self, block: u64, metadata: &BlockMetadata);

    /// The pump ended (WS stream dead or pump task exited): make the
    /// Python-facing streams END so the bot fails loudly. No default — every
    /// engine answers the liveness question explicitly, and the arb engine's
    /// answer carries the 2026-08-20 loud-close log.
    fn on_pump_ended(&self);
}
