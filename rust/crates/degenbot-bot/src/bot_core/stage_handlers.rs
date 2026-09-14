//! `StageHandlers` — the ONE engine seam of the block-epoch pipeline (epic
//! MROOY7, task YM2FZR; ADR-041 seam retirement #1).
//!
//! The stage table in `docs/architecture/block-epoch-pipeline.md` (canonical
//! copy on branch `pi-fabric/adr-041`, signed off) fixes the per-stage hook
//! set the runtime drives for every block epoch: streaming-complete
//! (Quiesced), resolve (Resolved), solve (Solved), simulate (Simulated),
//! gate (Gated), publish (Published), finalize (Finalized), and rewind
//! (`Rewind{to_epoch}`). This module encodes that table as a single trait.
//! The arb engine's existing logic becomes a `StageHandlers` implementation
//! in the stage-machine task (7NFYQW); the `NoopStubEngine` conformance stub
//! in this file's test support is the *other* implementer that keeps the
//! trait honest from day one (ADR-041 non-goals: no multi-engine machinery,
//! no registry, no runtime selection — the stub is a test-declared
//! conformance harness, never a configurable engine).
//!
//! # Compile-fails-if-incomplete
//!
//! The property has two halves:
//!
//! 1. **No default hook bodies.** Every hook is required; adding a stage hook
//!    to the trait without updating each implementer (including the stub's
//!    `impl StageHandlers` block, which lives in this file's test build) is a
//!    compile error. `cargo test -p degenbot-bot` therefore fails until the
//!    stub supports the new hook.
//! 2. **Exhaustive order table.** [`legal_successors`] matches over [`Stage`]
//!    without a wildcard and [`ALL_STAGES`] is a sized `const`: adding a
//!    stage variant without declaring its position in the cycle is a compile
//!    error too. The conformance harness then asserts every stage in
//!    [`ALL_STAGES`] is observed end-to-end.
//!
//! ## Input surface
//!
//! Each hook receives its [`BlockContext`] (the one epoch coordinate,
//! task T6IYKY, plus the block's execution metadata) plus the previous
//! stage's output. The per-epoch dirty tracking travels as the opaque
//! [`EpochDelta`] handle — the real touched-pool ledger
//! (`crate::bot_core::epoch_delta`, LXDY4C), re-exported by this module;
//! its internals are NOT
//! part of this seam, so the dirty-tracking rewrite cannot fork the trait.

use std::fmt;
#[cfg(test)]
use std::sync::atomic::{AtomicU8, Ordering};

use crate::bot_core::{BlockContext, Epoch};

// ----------------------------------------------------------------------
// Stage
// ----------------------------------------------------------------------

/// The per-stage hook set the runtime drives, in ADR-041 stage-table order.
///
/// [`Stage::Rewind`] is listed last for `Display` convenience only; in the
/// machine it can fire from ANY stage (invariant I6: at most one rewind in
/// flight), and it restarts the cycle at [`Stage::StreamingComplete`] on a
/// fresh epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// Quiesced row: all dispatched logs for the open block applied;
    /// completeness classified (tombstone-by-successor vs settle;
    /// early-slice/debounce gates). The only hook whose work item may carry
    /// a [`BackfillEpisode`] (gap `eth_getLogs` applied before quiescence).
    StreamingComplete,
    /// Resolved row: the block's epoch delta → affected-path derivation.
    /// The delta is read opaque; the derivation is `EpochDelta`-internal.
    Resolve,
    /// Solved row: the solver runs the affected paths.
    Solve,
    /// Simulated row: in-process revm simulation — the sole executor
    /// (ADR-019); tri-state profit facts per candidate (ADR-030).
    Simulate,
    /// Gated row: risk/size/profit rechecks; deliver/reject verdicts per
    /// bucket (ADR-040).
    Gate,
    /// Published row: winning bundle to execution; exactly one publish per
    /// quiesce cycle (invariant I5). Sinks subscribe at this edge.
    Publish,
    /// Finalized row: epoch closed; the delivery cutoff is stamped
    /// (monotone, never reset by a resume — invariant I7).
    Finalize,
    /// `Rewind{to_epoch}` row: reorg unwind from ANY stage to a fresh epoch
    /// at an earlier (or same-block) coordinate; `Epoch.seq` bumps exactly
    /// once (invariant I2); stale contexts fail fast (I3).
    Rewind,
}

/// Every stage hook, in cycle order. The conformance harness asserts an
/// implementer observes each of these across the scripted lifecycle.
pub const ALL_STAGES: [Stage; 8] = [
    Stage::StreamingComplete,
    Stage::Resolve,
    Stage::Solve,
    Stage::Simulate,
    Stage::Gate,
    Stage::Publish,
    Stage::Finalize,
    Stage::Rewind,
];

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Stage::StreamingComplete => "streaming-complete (quiesced)",
            Stage::Resolve => "resolve",
            Stage::Solve => "solve",
            Stage::Simulate => "simulate",
            Stage::Gate => "gate",
            Stage::Publish => "publish",
            Stage::Finalize => "finalize",
            Stage::Rewind => "rewind",
        };
        write!(f, "{name}")
    }
}

/// The stage-cycle successor table: which stages may legally follow
/// `previous` in a driven cycle. `None` is the fresh (no stage driven) state
/// — a cycle starts at [`Stage::StreamingComplete`]. A rewind is a legal
/// successor everywhere an epoch is open (I6: it may originate from any
/// stage); after a [`Stage::Rewind`] or a completed [`Stage::Finalize`], the
/// next legal stage is the next cycle's [`Stage::StreamingComplete`].
///
/// The match is exhaustive by design (no wildcard): adding a [`Stage`]
/// variant without declaring its successors does not compile — half of the
/// compile-fails-if-incomplete property.
#[must_use]
pub const fn legal_successors(previous: Option<Stage>) -> &'static [Stage] {
    match previous {
        None | Some(Stage::Rewind) => &[Stage::StreamingComplete],
        Some(Stage::StreamingComplete) => &[Stage::Resolve, Stage::Rewind],
        Some(Stage::Resolve) => &[Stage::Solve, Stage::Rewind],
        Some(Stage::Solve) => &[Stage::Simulate, Stage::Rewind],
        Some(Stage::Simulate) => &[Stage::Gate, Stage::Rewind],
        Some(Stage::Gate) => &[Stage::Publish, Stage::Rewind],
        Some(Stage::Publish) => &[Stage::Finalize, Stage::Rewind],
        Some(Stage::Finalize) => &[Stage::StreamingComplete, Stage::Rewind],
    }
}

// ----------------------------------------------------------------------
// Opaque delta handle
// ----------------------------------------------------------------------

// LXDY4C cutover (task 2UVG3E data-plane pass): the module-local placeholder
// is unified onto the REAL per-epoch dirty ledger (crate::bot_core::epoch_delta,
// the type Bot mints at dispatch time) — the StageHandlers signatures are
// unchanged, which is the point: dirty-tracking details cannot fork the
// engine seam.

#[doc(inline)]
pub use crate::bot_core::epoch_delta::EpochDelta;

/// A gap-backfill episode the Streaming stage executed before this epoch
/// quiesced (stage table: Streaming applies live WS logs + gap `eth_getLogs`
/// backfill in log order). Fed synthetically by the conformance harness —
/// no WebSocket, no RPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackfillEpisode {
    /// First block of the filled gap (inclusive).
    pub from_block: u64,
    /// Last block of the filled gap (inclusive).
    pub to_block: u64,
}

// ----------------------------------------------------------------------
// Stage data plane (small typed carriers; the stub never computes)
// ----------------------------------------------------------------------

/// The completeness verdict classified at streaming-complete (Quiesced row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuiesceVerdict {
    /// The block quiesced on its own header tick (settle path).
    Settled,
    /// The block was completed by its successor (tombstone-by-successor).
    Tombstoned,
}

/// The streaming-complete stage's output, forwarded to resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuiesceOutcome {
    /// The classified completeness verdict.
    pub verdict: QuiesceVerdict,
}

/// A solved-then-tracked candidate. The stub fabricates none; the arb engine
/// populates real ones when it becomes a `StageHandlers` implementer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CandidateId(pub u64);

/// The affected-path set derived at the Resolved row from the epoch's delta.
/// (SZJUKL: carries the real ledger keys — the `EpochDelta` take — not loose
/// ids, so the Solved hook receives exactly what the solver consumes.)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AffectedPaths(pub Vec<degenbot_solvers::affected_keys::AffectedKey>);

/// The Solved row's output: the cursor fact AND the product. The seam now
/// carries the epoch the cycle was anchored at ('solved', required - a cycle
/// always knows its anchor) alongside the candidates risen from the affected
/// paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveOutcome {
    /// Candidates the solver produced for this epoch.
    pub candidates: Vec<CandidateId>,
    /// The epoch the solve cycle was anchored at. Required, never optional:
    /// a cycle always knows its anchor, and the driver derives the engine
    /// cursor from this fact rather than re-poking the seam.
    pub solved: Epoch,
}

impl Default for SolveOutcome {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            solved: Epoch::at(0),
        }
    }
}

/// Tri-state simulation verdict (ADR-030).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimVerdict {
    /// Profitable in-process: route onward.
    Deliver,
    /// Simulated, not profitable enough: do not deliver.
    DontDeliver,
    /// Simulation could not establish a verdict (derivation/profit fact).
    Invalid,
}

/// One candidate's simulation fact (Simulated row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimFact {
    /// The simulated candidate.
    pub candidate: CandidateId,
    /// Its tri-state verdict.
    pub verdict: SimVerdict,
}

/// The Simulated row's output, forwarded to the Gated row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimulateOutcome {
    /// Per-candidate simulation facts.
    pub facts: Vec<SimFact>,
}

/// Deliver / reject verdict (Gated row; ADR-040).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateVerdict {
    /// Cleared all rechecks: eligible at the Published edge.
    Deliver,
    /// Rejected by at least one recheck.
    Reject,
}

/// A gated candidate's verdict (Gated row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatedCandidate {
    /// The candidate that was gated.
    pub candidate: CandidateId,
    /// Its deliver/reject verdict.
    pub verdict: GateVerdict,
}

/// The Gated row's output, forwarded to the Published row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateOutcome {
    /// Per-candidate gating verdicts.
    pub verdicts: Vec<GatedCandidate>,
}

/// The Published row's output: the winning bundle delivered to execution
/// sinks (exactly ≤1 per quiesce cycle, I5). Written to the delivery/FFI
/// surface only — never pool state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublishOutcome {
    /// The published candidate, if the cycle produced one.
    pub published: Option<CandidateId>,
}

/// The Finalized row's output: the delivery cutoff stamp. Monotone per
/// invariant I7; never reset by a resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizeOutcome {
    /// The stamped cutoff (the closing epoch).
    pub cutoff: Epoch,
}

/// The Rewind row's outcome: the epoch the machine was restored to
/// (generation bumped, head at the post-reorg block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RewindOutcome {
    /// The fresh epoch the machine continues from.
    pub restored_to: Epoch,
}

// ----------------------------------------------------------------------
// Work items (one per hook; the runtime builds them)
// ----------------------------------------------------------------------

/// Streaming-complete (Quiesced) work item.
pub struct StreamingComplete<'a> {
    /// The epoch that quiesced.
    pub ctx: BlockContext,
    /// The epoch's dirty-tracking handle (opaque; LXDY4C owns internals).
    pub delta: &'a EpochDelta,
    /// The gap-backfill episode applied during Streaming, if any.
    pub backfill: Option<BackfillEpisode>,
}

/// Resolve (Resolved) work item.
pub struct Resolve<'a> {
    /// The epoch being resolved.
    pub ctx: BlockContext,
    /// The quiescence verdict that opened this cycle.
    pub quiesced: &'a QuiesceOutcome,
    /// The epoch's dirty-tracking handle (opaque).
    pub delta: &'a EpochDelta,
}

/// Solve (Solved) work item.
pub struct Solve {
    /// The epoch being solved.
    pub ctx: BlockContext,
    /// The affected-path set from the Resolved row.
    pub paths: AffectedPaths,
}

/// Simulate (Simulated) work item.
pub struct Simulate {
    /// The epoch being simulated.
    pub ctx: BlockContext,
    /// The solved candidates.
    pub solved: SolveOutcome,
}

/// Gate (Gated) work item.
pub struct Gate {
    /// The epoch being gated.
    pub ctx: BlockContext,
    /// The simulation facts.
    pub facts: SimulateOutcome,
}

/// Publish (Published) work item.
pub struct Publish {
    /// The epoch being published.
    pub ctx: BlockContext,
    /// The gating verdicts.
    pub gated: GateOutcome,
}

/// Finalize (Finalized) work item.
pub struct Finalize {
    /// The epoch being closed.
    pub ctx: BlockContext,
}

/// Rewind work item.
pub struct Rewind {
    /// The fresh epoch to unwind to (generation bumped by exactly one, I2).
    pub to_epoch: Epoch,
}

// ----------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------

/// Stage hook error / control-flow outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageError {
    /// I6 control flow: a stage requested a rewind from mid-cycle. The
    /// runtime must abandon the open epoch and drive the [`Stage::Rewind`]
    /// hook to `to_epoch` (generation bumps exactly once, I2). At most one
    /// rewind may be in flight; a second fails fast.
    RewindRequested {
        /// The fresh epoch to rewind to.
        to_epoch: Epoch,
    },
    /// A hard hook failure. The stage-machine task (7NFYQW) decides the
    /// runtime posture (fail-loud per ADR-021) — never a silent skip.
    Failed {
        /// The hook that failed.
        stage: Stage,
    },
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageError::RewindRequested { to_epoch } => write!(
                f,
                "stage requested a rewind to block {} @ gen {}",
                to_epoch.block(),
                to_epoch.seq()
            ),
            StageError::Failed { stage } => write!(f, "stage {stage} failed"),
        }
    }
}

impl std::error::Error for StageError {}

/// I6 bookkeeping: at most one rewind in flight; a second request fails
/// fast instead of interleaving reorg episodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RewindTracker {
    /// The open rewind's target epoch, if one is in flight.
    in_flight: Option<Epoch>,
}

impl RewindTracker {
    /// Record that a rewind to `to_epoch` has been requested. Fails fast if
    /// a rewind is already in flight (invariant I6).
    ///
    /// # Errors
    /// [`DoubleRewind`] when a rewind is already in flight.
    pub fn open(&mut self, to_epoch: Epoch) -> Result<Epoch, DoubleRewind> {
        if let Some(in_flight) = self.in_flight {
            return Err(DoubleRewind {
                in_flight,
                requested: to_epoch,
            });
        }
        self.in_flight = Some(to_epoch);
        Ok(to_epoch)
    }

    /// Complete the in-flight rewind; returns its target epoch.
    pub fn settle(&mut self) -> Option<Epoch> {
        self.in_flight.take()
    }

    /// Is a rewind currently in flight?
    #[must_use]
    pub const fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }
}

/// A second rewind was requested while one was in flight — I6 forbids it
/// and the request must fail fast, never queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoubleRewind {
    /// The rewind already in flight.
    pub in_flight: Epoch,
    /// The rejected second request.
    pub requested: Epoch,
}

impl fmt::Display for DoubleRewind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a rewind to block {} @ gen {} is already in flight; \
             rejecting the second rewind to block {} @ gen {} (I6: at most one in flight)",
            self.in_flight.block(),
            self.in_flight.seq(),
            self.requested.block(),
            self.requested.seq()
        )
    }
}

impl std::error::Error for DoubleRewind {}

// ----------------------------------------------------------------------
// The seam
// ----------------------------------------------------------------------

/// The ONE engine seam (ADR-041 seam retirement #1, replacing the
/// `DrainSink`/`Engine` dual seam): the per-stage hook set the runtime
/// drives, in stage-table order, for every block epoch.
///
/// Implementors: the arb engine (stage-machine task 7NFYQW) and — in this
/// file's test build — the `NoopStubEngine` conformance stub, which keeps
/// the trait honest from day one. There is no registry and no runtime
/// selection: a future second engine implements this trait or there is no
/// second engine.
///
/// Every hook is a required method with no default body: adding a hook to
/// this trait without implementing it breaks both implementers' builds (the
/// compile-fails-if-incomplete property). Hooks never compute — they carry
/// the stage's decision to the runtime, which owns ordering, state posture,
/// and stage transitions.
pub trait StageHandlers: Send + Sync {
    /// Quiesced row: all dispatched logs for the epoch's block are applied;
    /// classify completeness (tombstone-by-successor vs settle; the
    /// early-slice/debounce gates).
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_streaming_complete(
        &self,
        work: &StreamingComplete<'_>,
    ) -> Result<QuiesceOutcome, StageError>;

    /// Resolved row: derive the affected-path set from the epoch's delta.
    /// The delta arrives opaque — the derivation is `EpochDelta`-internal.
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_resolve(&self, work: &Resolve<'_>) -> Result<AffectedPaths, StageError>;

    /// Solved row: run the solver over the affected paths.
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_solve(&self, work: &Solve) -> Result<SolveOutcome, StageError>;

    /// Simulated row: in-process revm simulation — the sole executor
    /// (ADR-019); tri-state facts per candidate (ADR-030).
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_simulate(&self, work: &Simulate) -> Result<SimulateOutcome, StageError>;

    /// Gated row: risk/size/profit rechecks; deliver/reject verdicts per
    /// bucket (ADR-040). Pure evaluation — no state access.
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_gate(&self, work: &Gate) -> Result<GateOutcome, StageError>;

    /// Published row: deliver the winning bundle to execution; exactly one
    /// publish per quiesce cycle (I5). Sinks subscribe at this edge — the
    /// RPC-disagreement verify (ADR-021) rides here, not before the engine.
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_publish(&self, work: &Publish) -> Result<PublishOutcome, StageError>;

    /// Finalized row: stamp the delivery cutoff for the closed epoch
    /// (monotone, never reset by a resume — I7).
    ///
    /// # Errors
    /// [`StageError::RewindRequested`] on a mid-cycle rewind request;
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_finalize(&self, work: &Finalize) -> Result<FinalizeOutcome, StageError>;

    /// `Rewind{to_epoch}` row: reorg unwind from any stage to a fresh epoch
    /// at the post-reorg head. `to_epoch.seq` bumps exactly once per rewind
    /// (I2); every context minted in the previous generation must fail
    /// [`Epoch::ensure_current`] from here on (I3).
    ///
    /// # Errors
    /// [`StageError::Failed`] on a hard hook failure.
    fn on_rewind(&self, work: &Rewind) -> Result<RewindOutcome, StageError>;
}

// ======================================================================
// TEST SUPPORT — everything below this line is TEST-DECLARED ONLY
// (`#[cfg(test)]`): the conformance harness and the NoopStubEngine are
// never runtime-selectable (ADR-041 non-goals, Q4 decision).
// ======================================================================

#[cfg(test)]
mod conformance {
    //! Conformance harness for `StageHandlers` implementers — a scripted
    //! synthetic block stream (no WS, no RPC) driving the full lifecycle,
    //! including a reorg episode and a backfill episode, asserting stage
    //! order, hook completeness, epoch monotonicity (I2), stale-context
    //! rejection (I3), exactly-one-publish-per-cycle (I5), double-rewind
    //! fail-fast (I6), and delivery-cutoff monotonicity (I7).

    use super::*;

    /// A scripted synthetic-stream step.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum StreamEvent {
        /// A live epoch at `block` quiesced (full non-rewind cycle driven).
        Epoch {
            /// The block coordinate of the new epoch.
            block: u64,
        },
        /// A gap-backfill episode filled `[from, to]` (inclusive) before
        /// the epoch at `block` quiesced (Streaming row's gap `eth_getLogs`
        /// path).
        BackfillThenEpoch {
            /// The block coordinate of the new epoch.
            block: u64,
            /// The filled gap (inclusive bounds).
            gap: (u64, u64),
        },
        /// A reorg landed; the runtime unwinds to a fresh epoch at
        /// `to_block` (stream-detected reorg — the generation bumps exactly
        /// once, I2).
        Reorg {
            /// The post-reorg head block.
            to_block: u64,
        },
    }

    /// What went wrong in a conformance run. A conformance failure is itself
    /// the assertion — the harness proves nothing if a violation slips past.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ConformanceError {
        /// A hook was invoked outside [`legal_successors`] order.
        StageOrder {
            /// The illegal stage attempted.
            got: Stage,
            /// The stage order that was legal at that point.
            expected: &'static [Stage],
        },
        /// A stage was driven with no open epoch.
        NoOpenEpoch {
            /// The stage that was driven.
            stage: Stage,
        },
        /// The epoch coordinate regressed without a rewind (I2).
        EpochRegressed {
            /// The epoch in flight.
            from: Epoch,
            /// The regressed epoch.
            to: Epoch,
        },
        /// A rewind did not bump the generation by exactly one (I2).
        RewindSeqSkipped {
            /// The current generation before the rewind.
            from: Epoch,
            /// The rewind target (its `seq` moved by more than one).
            to_epoch: Epoch,
        },
        /// A stale context was accepted for the current epoch (I3).
        StaleContextAccepted {
            /// The context minted in the pre-rewind generation.
            held: Epoch,
            /// The current epoch after the rewind.
            current: Epoch,
        },
        /// A second rewind was requested while one was in flight (I6).
        DoubleRewind {
            /// The rewind already in flight.
            in_flight: Epoch,
            /// The rejected second request.
            requested: Epoch,
        },
        /// Exactly-one-publish-per-cycle violated (I5).
        PublishCount {
            /// Publishes observed in the offending cycle.
            count: usize,
        },
        /// The delivery cutoff regressed or failed to advance (I7).
        CutoffRegressed {
            /// The previous cutoff.
            previous: Epoch,
            /// The stamped cutoff.
            stamped: Epoch,
        },
        /// A hook's outcome did not echo the epoch being driven (I1: the
        /// epoch is the only block coordinate).
        EpochEchoMismatch {
            /// The hook that mis-echoed.
            stage: Stage,
            /// The epoch the hook carried back.
            got: Option<Epoch>,
            /// The epoch the runner held.
            expected: Epoch,
        },
        /// A declared stage hook was never exercised (hook completeness —
        /// the stub must implement AND observe every hook).
        HookMissing {
            /// The hook that was never driven.
            stage: Stage,
        },
        /// A hook failed with a non-control-flow error (the stub's contract
        /// is to always succeed: it never computes).
        HookFailed {
            /// The hook that failed.
            stage: Stage,
            /// The failure it returned.
            error: StageError,
        },
        /// A mid-cycle hook requested a rewind; the cycle was abandoned and
        /// the rewind executed (I6 control flow — legal, not a failure).
        CycleAbortedByRewind,
    }

    impl fmt::Display for ConformanceError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                ConformanceError::StageOrder { got, expected } => {
                    write!(f, "stage {got} driven out of order; legal: {expected:?}")
                }
                ConformanceError::NoOpenEpoch { stage } => {
                    write!(f, "stage {stage} driven with no open epoch")
                }
                ConformanceError::EpochRegressed { from, to } => {
                    write!(f, "epoch regressed {from:?} -> {to:?} without a rewind (I2)")
                }
                ConformanceError::RewindSeqSkipped { from, to_epoch } => write!(
                    f,
                    "rewind target {to_epoch:?} does not bump the generation of {from:?} by exactly one (I2)"
                ),
                ConformanceError::StaleContextAccepted { held, current } => write!(
                    f,
                    "stale context {held:?} accepted for current epoch {current:?} (I3)"
                ),
                ConformanceError::DoubleRewind { in_flight, requested } => write!(
                    f,
                    "second rewind to {requested:?} while {in_flight:?} in flight (I6)"
                ),
                ConformanceError::PublishCount { count } => {
                    write!(f, "a quiesce cycle published {count} times (I5: at most one)")
                }
                ConformanceError::CutoffRegressed { previous, stamped } => {
                    write!(f, "delivery cutoff moved {previous:?} -> {stamped:?} (I7: monotone)")
                }
                ConformanceError::EpochEchoMismatch { stage, got, expected } => {
                    write!(f, "hook {stage} echoed {got:?}; the runner held {expected:?}")
                }
                ConformanceError::HookFailed { stage, error } => {
                    write!(f, "hook {stage} failed: {error}")
                }
                ConformanceError::CycleAbortedByRewind => {
                    write!(f, "cycle abandoned by a mid-cycle rewind (I6 control flow)")
                }
                ConformanceError::HookMissing { .. } => {
                    write!(f, "a declared stage hook was never observed")
                }
            }
        }
    }

    impl std::error::Error for ConformanceError {}

    /// One hook invocation observed in the drive trace.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TraceEntry {
        /// The hook invoked.
        pub stage: Stage,
        /// The epoch the runner held when the hook was driven.
        pub epoch: Epoch,
        /// The backfill episode attached to the streaming-complete hook, if
        /// any.
        pub backfill: Option<BackfillEpisode>,
    }

    /// The conformance driving order — the thin stand-in for the future
    /// stage machine (task 7NFYQW). It drives one `E: StageHandlers`
    /// implementer over a scripted synthetic block stream and asserts the
    /// executable-spec properties end to end.
    struct Harness<'a, E: ?Sized> {
        engine: &'a E,
        /// The epoch the machine currently holds. `None` until the first
        /// script event seeds it at generation 0.
        current: Option<Epoch>,
        /// The epoch the last rewind landed on (a cycle resuming exactly at
        /// it is legal; any other epoch must strictly advance, I2).
        resumed_at: Option<Epoch>,
        /// The context of the most recent cycle (I3 stale-context probe).
        last_ctx: Option<Epoch>,
        /// The last legally driven stage (cycle-order cursor).
        predecessor: Option<Stage>,
        /// Every hook invocation, in drive order.
        trace: Vec<TraceEntry>,
        /// The delivery cutoff (I7) — the last Finalized epoch.
        cutoff: Option<Epoch>,
        /// I6: at most one rewind in flight.
        rewinds: RewindTracker,
        /// The opaque per-epoch delta handle passed through to the hooks.
        delta: EpochDelta,
    }

    impl<'a, E: StageHandlers + ?Sized> Harness<'a, E> {
        fn new(engine: &'a E) -> Self {
            Self {
                engine,
                current: None,
                resumed_at: None,
                last_ctx: None,
                predecessor: None,
                trace: Vec::new(),
                cutoff: None,
                rewinds: RewindTracker::default(),
                delta: EpochDelta::new(0u64),
            }
        }

        /// Drive a scripted synthetic block stream to completion, then run
        /// the full conformance assertions over the observed trace.
        fn run_stream(&mut self, script: &[StreamEvent]) -> Result<(), ConformanceError> {
            for event in script {
                let outcome = match *event {
                    StreamEvent::Epoch { block } => self.run_epoch(block, None),
                    StreamEvent::BackfillThenEpoch { block, gap } => self.run_epoch(
                        block,
                        Some(BackfillEpisode {
                            from_block: gap.0,
                            to_block: gap.1,
                        }),
                    ),
                    StreamEvent::Reorg { to_block } => self.run_rewind(to_block),
                };
                // A mid-cycle rewind abandoning its cycle (I6) is legal —
                // the rewind side effects are applied; the stream continues.
                if let Err(ConformanceError::CycleAbortedByRewind) = outcome {
                    continue;
                }
                outcome?;
            }
            self.assert_conformance()
        }

        /// The order gate (compile-fails-if-incomplete's runtime twin): the
        /// stage must be a legal successor of the last driven stage.
        fn advance(
            &mut self,
            stage: Stage,
            backfill: Option<BackfillEpisode>,
        ) -> Result<(), ConformanceError> {
            let epoch = self
                .current
                .ok_or(ConformanceError::NoOpenEpoch { stage })?;
            let expected = legal_successors(self.predecessor);
            if !expected.contains(&stage) {
                return Err(ConformanceError::StageOrder {
                    got: stage,
                    expected,
                });
            }
            self.trace.push(TraceEntry {
                stage,
                epoch,
                backfill,
            });
            self.predecessor = Some(stage);
            Ok(())
        }

        /// Drive one full cycle (all non-rewind stages) for `block`. A
        /// mid-cycle `RewindRequested` from the engine aborts the cycle into
        /// a rewind.
        fn run_epoch(
            &mut self,
            block: u64,
            backfill: Option<BackfillEpisode>,
        ) -> Result<(), ConformanceError> {
            let epoch = match self.current {
                Some(current) => {
                    let candidate = Epoch::with_generation(block, current.seq());
                    // A cycle may resume exactly at the rewind target;
                    // otherwise the coordinate must strictly advance (I2).
                    if self.resumed_at != Some(candidate) && candidate <= current {
                        return Err(ConformanceError::EpochRegressed {
                            from: current,
                            to: candidate,
                        });
                    }
                    self.resumed_at = None;
                    candidate
                }
                None => Epoch::at(block),
            };
            self.current = Some(epoch);
            let ctx = BlockContext::new(epoch, Self::meta(block));
            self.last_ctx = Some(epoch);

            self.advance(Stage::StreamingComplete, backfill)?;
            let quiesced = self.drive(Stage::StreamingComplete, epoch, |engine, delta| {
                engine.on_streaming_complete(&StreamingComplete {
                    ctx,
                    delta,
                    backfill,
                })
            })?;

            self.advance(Stage::Resolve, None)?;
            let paths = self.drive(Stage::Resolve, epoch, |engine, delta| {
                engine.on_resolve(&Resolve {
                    ctx,
                    quiesced: &quiesced,
                    delta,
                })
            })?;

            self.advance(Stage::Solve, None)?;
            let solved = self.drive(Stage::Solve, epoch, |engine, _| {
                engine.on_solve(&Solve { ctx, paths })
            })?;

            self.advance(Stage::Simulate, None)?;
            let facts = self.drive(Stage::Simulate, epoch, |engine, _| {
                engine.on_simulate(&Simulate { ctx, solved })
            })?;

            self.advance(Stage::Gate, None)?;
            let gated = self.drive(Stage::Gate, epoch, |engine, _| {
                engine.on_gate(&Gate { ctx, facts })
            })?;

            self.advance(Stage::Publish, None)?;
            let _published = self.drive(Stage::Publish, epoch, |engine, _| {
                engine.on_publish(&Publish { ctx, gated })
            })?;

            self.advance(Stage::Finalize, None)?;
            let finalized = self.drive(Stage::Finalize, epoch, |engine, _| {
                engine.on_finalize(&Finalize { ctx })
            })?;
            if finalized.cutoff != epoch {
                return Err(ConformanceError::EpochEchoMismatch {
                    stage: Stage::Finalize,
                    got: Some(finalized.cutoff),
                    expected: epoch,
                });
            }
            if let Some(previous) = self.cutoff {
                if finalized.cutoff <= previous {
                    return Err(ConformanceError::CutoffRegressed {
                        previous,
                        stamped: finalized.cutoff,
                    });
                }
            }
            self.cutoff = Some(finalized.cutoff);
            Ok(())
        }

        /// Drive one hook, mapping engine control flow (a mid-cycle
        /// `RewindRequested`) and hard failure into conformance errors.
        fn drive<T>(
            &mut self,
            stage: Stage,
            held: Epoch,
            work: impl FnOnce(&E, &EpochDelta) -> Result<T, StageError>,
        ) -> Result<T, ConformanceError> {
            match work(self.engine, &self.delta) {
                Ok(value) => Ok(value),
                Err(StageError::RewindRequested { to_epoch }) => {
                    // Legal I6 control flow: the cycle is abandoned here and
                    // the rewind executed; the caller unwinds with the
                    // sentinel and the stream continues at the new epoch.
                    self.abort_to_rewind(to_epoch, held)?;
                    Err(ConformanceError::CycleAbortedByRewind)
                }
                Err(error) => Err(ConformanceError::HookFailed { stage, error }),
            }
        }

        /// Execute a stream-detected rewind to `to_block`: the generation
        /// bumps by exactly one (I2), every context minted in the previous
        /// generation goes stale (I3), and at most one rewind may be in
        /// flight (I6).
        fn run_rewind(&mut self, to_block: u64) -> Result<(), ConformanceError> {
            let current = self.current.ok_or(ConformanceError::NoOpenEpoch {
                stage: Stage::Rewind,
            })?;
            let held = self.last_ctx;
            let to_epoch = current.rewind_to(to_block);
            self.open_rewind(current, to_epoch)?;
            self.execute_rewind(to_epoch)?;

            // I3: a context minted in the pre-rewind generation must fail
            // ensure_current against the fresh epoch — fail fast, never a
            // silent apply of a rewound chain view.
            if let Some(held) = held {
                if held.ensure_current(to_epoch).is_ok() {
                    return Err(ConformanceError::StaleContextAccepted {
                        held,
                        current: to_epoch,
                    });
                }
            }
            self.current = Some(to_epoch);
            self.resumed_at = Some(to_epoch);
            Ok(())
        }

        /// Rewind requested from a mid-cycle hook (I6: a rewind may
        /// originate from ANY stage): abandon the open cycle and unwind.
        fn abort_to_rewind(
            &mut self,
            to_epoch: Epoch,
            held: Epoch,
        ) -> Result<(), ConformanceError> {
            let current = self.current.ok_or(ConformanceError::NoOpenEpoch {
                stage: Stage::Rewind,
            })?;
            self.open_rewind(current, to_epoch)?;
            self.execute_rewind(to_epoch)?;

            // I3: the abandoned work context must go stale against the new
            // epoch — the generation moved, so this is guaranteed to fail;
            // if it does NOT, the_coordinate model is broken.
            if held.ensure_current(to_epoch).is_ok() {
                return Err(ConformanceError::StaleContextAccepted {
                    held,
                    current: to_epoch,
                });
            }
            self.current = Some(to_epoch);
            self.resumed_at = Some(to_epoch);
            Ok(())
        }

        fn open_rewind(&mut self, current: Epoch, to_epoch: Epoch) -> Result<(), ConformanceError> {
            if let Err(e) = self.rewinds.open(to_epoch) {
                return Err(ConformanceError::DoubleRewind {
                    in_flight: e.in_flight,
                    requested: e.requested,
                });
            }
            if to_epoch.seq() != current.seq() + 1 {
                return Err(ConformanceError::RewindSeqSkipped {
                    from: current,
                    to_epoch,
                });
            }
            Ok(())
        }

        fn execute_rewind(&mut self, to_epoch: Epoch) -> Result<(), ConformanceError> {
            self.advance(Stage::Rewind, None)?;
            let rewind = Rewind { to_epoch };
            let outcome =
                self.engine
                    .on_rewind(&rewind)
                    .map_err(|error| ConformanceError::HookFailed {
                        stage: Stage::Rewind,
                        error,
                    })?;
            if outcome.restored_to != to_epoch {
                return Err(ConformanceError::EpochEchoMismatch {
                    stage: Stage::Rewind,
                    got: Some(outcome.restored_to),
                    expected: to_epoch,
                });
            }
            self.rewinds.settle();
            Ok(())
        }

        /// The executable-spec assertions over the observed trace.
        fn assert_conformance(&self) -> Result<(), ConformanceError> {
            // Hook completeness: every declared stage must be exercised.
            for stage in ALL_STAGES {
                if !self.trace.iter().any(|entry| entry.stage == stage) {
                    return Err(ConformanceError::HookMissing { stage });
                }
            }

            // I5: exactly one publish per quiesce cycle (publish and
            // finalize counts must agree; no cycle may publish twice).
            let mut publishes_in_cycle = 0usize;
            let mut publishes_total = 0usize;
            let mut finalizes_total = 0usize;
            for entry in &self.trace {
                match entry.stage {
                    Stage::StreamingComplete => {
                        if publishes_in_cycle > 1 {
                            return Err(ConformanceError::PublishCount {
                                count: publishes_in_cycle,
                            });
                        }
                        publishes_in_cycle = 0;
                    }
                    Stage::Publish => {
                        publishes_in_cycle += 1;
                        publishes_total += 1;
                    }
                    Stage::Finalize => finalizes_total += 1,
                    _ => {}
                }
            }
            // The trailing cycle has no closing StreamingComplete; its
            // per-cycle count (and the global publish/finalize agreement)
            // must still hold. A mid-cycle rewind may legitimately leave a
            // final cycle with zero publishes.
            if publishes_in_cycle > 1 || publishes_total != finalizes_total {
                return Err(ConformanceError::PublishCount {
                    count: publishes_in_cycle,
                });
            }

            // I2 epoch monotonicity walk: within a cycle the epoch is
            // constant; across cycles it strictly advances (generation-
            // dominant Ord allows the block to regress after a rewind);
            // the rewind record itself carries the pre-rewind coordinate.
            let mut rewind_pending = false;
            let mut previous: Option<Epoch> = None;
            for entry in &self.trace {
                if entry.stage == Stage::Rewind {
                    rewind_pending = true;
                    continue;
                }
                let epoch = entry.epoch;
                if let Some(from) = previous {
                    if rewind_pending {
                        // Post-rewind: block may regress; the generation-
                        // dominant Ord must still rank the new epoch above.
                        if epoch <= from {
                            return Err(ConformanceError::EpochRegressed { from, to: epoch });
                        }
                    } else if epoch.seq() < from.seq()
                        || (epoch.seq() == from.seq() && epoch.block() < from.block())
                    {
                        return Err(ConformanceError::EpochRegressed { from, to: epoch });
                    }
                }
                rewind_pending = false;
                previous = Some(epoch);
            }
            Ok(())
        }

        /// The observed hook trace (test-inspection surface).
        fn trace(&self) -> &[TraceEntry] {
            &self.trace
        }

        /// Synthetic block metadata for the harness (fees/gas/timestamp).
        fn meta(block: u64) -> crate::bot_core::BlockMetadata {
            crate::bot_core::BlockMetadata {
                timestamp: block * 1_000,
                base_fee_per_gas: Some(block),
                gas_used: 1,
                gas_limit: 2,
            }
        }
    }

    /// The `ALL_STAGES` index (the stub's atomic script encoding).
    #[expect(
        clippy::expect_used,
        reason = "scripted stub: a stage missing from ALL_STAGES is a conformance bug that must fail loudly"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "ALL_STAGES has well under 256 rows"
    )]
    fn stage_index(stage: Stage) -> u8 {
        ALL_STAGES
            .iter()
            .position(|s| *s == stage)
            .expect("scripted stage must be in ALL_STAGES") as u8
    }

    /// The executable spec of hook completeness (ADR-041 non-goals): a
    /// TEST-DECLARED conformance stub implementing every `StageHandlers`
    /// hook. It computes nothing — each hook returns a well-formed inert
    /// outcome and honors a one-shot scripted mid-cycle rewind request (so
    /// the harness can exercise a stage-initiated reorg). Never runtime-
    /// selectable: it is `#[cfg(test)]`-gated and there is no registry.
    #[derive(Debug, Default)]
    pub struct NoopStubEngine {
        /// The stage that will request a mid-cycle rewind, one-shot (cleared
        /// on firing). `0` = the all-inert script; otherwise the `ALL_STAGES`
        /// index + 1 (an atomic so the stub is `Send + Sync` like the trait
        /// requires).
        rewind_from: AtomicU8,
    }

    impl NoopStubEngine {
        /// An all-inert stub.
        #[must_use]
        pub fn new() -> Self {
            Self {
                rewind_from: AtomicU8::new(0),
            }
        }

        /// Script a one-shot mid-cycle rewind request from `stage`.
        #[must_use]
        pub fn rewind_from(self, stage: Stage) -> Self {
            self.rewind_from
                .store(stage_index(stage) + 1, Ordering::SeqCst);
            self
        }

        /// The one-shot control-flow probe: the scripted stage fires
        /// exactly one rewind request, then the stub reverts to inert.
        /// (An atomic consume: only the matching nonzero script index
        /// clears — the all-inert `0` never matches.)
        fn take_rewind(&self, stage: Stage) -> bool {
            let wanted = stage_index(stage) + 1;
            let stored = self.rewind_from.load(Ordering::SeqCst);
            if wanted != 0 && stored == wanted {
                // consume the one-shot script (non-matching probes leave it)
                self.rewind_from.store(0, Ordering::SeqCst);
                return true;
            }
            false
        }
    }

    impl StageHandlers for NoopStubEngine {
        fn on_streaming_complete(
            &self,
            work: &StreamingComplete<'_>,
        ) -> Result<QuiesceOutcome, StageError> {
            if self.take_rewind(Stage::StreamingComplete) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(QuiesceOutcome {
                verdict: QuiesceVerdict::Settled,
            })
        }

        fn on_resolve(&self, work: &Resolve<'_>) -> Result<AffectedPaths, StageError> {
            if self.take_rewind(Stage::Resolve) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(AffectedPaths(Vec::new()))
        }

        fn on_solve(&self, work: &Solve) -> Result<SolveOutcome, StageError> {
            if self.take_rewind(Stage::Solve) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(SolveOutcome {
                candidates: Vec::new(),
                solved: work.ctx.epoch(),
            })
        }

        fn on_simulate(&self, work: &Simulate) -> Result<SimulateOutcome, StageError> {
            if self.take_rewind(Stage::Simulate) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(SimulateOutcome { facts: Vec::new() })
        }

        fn on_gate(&self, work: &Gate) -> Result<GateOutcome, StageError> {
            if self.take_rewind(Stage::Gate) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(GateOutcome {
                verdicts: Vec::new(),
            })
        }

        fn on_publish(&self, work: &Publish) -> Result<PublishOutcome, StageError> {
            if self.take_rewind(Stage::Publish) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(PublishOutcome {
                published: Some(CandidateId(1)),
            })
        }

        fn on_finalize(&self, work: &Finalize) -> Result<FinalizeOutcome, StageError> {
            if self.take_rewind(Stage::Finalize) {
                return Err(StageError::RewindRequested {
                    to_epoch: work.ctx.epoch().rewind(),
                });
            }
            Ok(FinalizeOutcome {
                cutoff: work.ctx.epoch(),
            })
        }

        fn on_rewind(&self, work: &Rewind) -> Result<RewindOutcome, StageError> {
            Ok(RewindOutcome {
                restored_to: work.to_epoch,
            })
        }
    }

    // Lifecycle surface — the stub computes nothing and owns no channels.
    // Separate required trait (ADR-046): the pokes are the driver's control
    // seam, never stage hooks.
    impl crate::bot_core::PumpControl for NoopStubEngine {
        fn has_dirty_paths(&self) -> bool {
            false
        }

        fn set_last_solved_block(&self, _solved: Epoch) {}

        fn set_solve_anchor(&self, _anchor: Epoch) {}

        fn record_logs_this_block(&self) {}

        fn last_processed_block(&self) -> Option<Epoch> {
            None
        }

        fn notify_block(&self, _block: u64, _metadata: &crate::bot_core::BlockMetadata) {}

        fn on_pump_ended(&self) {}
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The full synthetic lifecycle: a normal stream, a gap-backfill
        /// episode, a stream-detected reorg (rewind), then the resumed
        /// stream in the new generation — asserting stage order, hook
        /// completeness, epoch monotonicity, and the scripted coordinates
        /// end to end.
        #[test]
        fn full_lifecycle_with_backfill_episode_and_reorg_passes() {
            let engine = NoopStubEngine::new();
            let mut harness = Harness::new(&engine);
            let script = [
                StreamEvent::Epoch { block: 100 },
                StreamEvent::Epoch { block: 101 },
                StreamEvent::BackfillThenEpoch {
                    block: 102,
                    gap: (101, 101),
                },
                StreamEvent::Reorg { to_block: 100 },
                StreamEvent::Epoch { block: 100 },
                StreamEvent::Epoch { block: 101 },
                StreamEvent::Epoch { block: 102 },
            ];
            assert!(
                harness.run_stream(&script).is_ok(),
                "conformance run must pass"
            );

            let trace = harness.trace();
            // 6 full cycles (7 hooks each) + 1 rewind record.
            assert_eq!(trace.len(), 6 * 7 + 1);
            assert_eq!(count(trace, Stage::StreamingComplete), 6);
            assert_eq!(count(trace, Stage::Publish), 6);
            assert_eq!(count(trace, Stage::Finalize), 6);
            assert_eq!(count(trace, Stage::Rewind), 1);

            // The one backfill episode surfaced, attached to its
            // streaming-complete hook, carrying the scripted gap.
            let backfills: Vec<_> = trace.iter().filter_map(|e| e.backfill).collect();
            assert_eq!(
                backfills,
                vec![BackfillEpisode {
                    from_block: 101,
                    to_block: 101
                }]
            );

            // The rewind record carries the PRE-rewind epoch (102 @ gen 0);
            // the resumed cycle runs at the fork block @ the bumped gen,
            // which sorts strictly ABOVE the pre-rewind epoch (I2 Ord).
            let rewind_pos = trace.iter().position(|e| e.stage == Stage::Rewind);
            assert_eq!(
                rewind_pos.map(|p| trace[p].epoch),
                Some(Epoch::with_generation(102, 0))
            );
            assert_eq!(
                rewind_pos.map(|p| trace[p + 1].epoch),
                Some(Epoch::with_generation(100, 1))
            );
            assert!(rewind_pos.map(|p| trace[p + 1].epoch) > Some(Epoch::with_generation(102, 0)));
        }

        /// I6: a rewind may originate from ANY stage — here the solve stage
        /// requests a mid-cycle rewind; the cycle abandons, the generation
        /// bumps exactly once, and the next epoch runs a complete cycle.
        #[test]
        fn rewind_requested_from_solve_aborts_the_cycle_and_bumps_seq_once() {
            let engine = NoopStubEngine::new().rewind_from(Stage::Solve);
            let mut harness = Harness::new(&engine);
            let script = [
                StreamEvent::Epoch { block: 200 },
                StreamEvent::Epoch { block: 201 },
            ];
            assert!(
                harness.run_stream(&script).is_ok(),
                "stage-initiated rewind must be handled"
            );

            let trace = harness.trace();
            let seen: Vec<Stage> = trace.iter().map(|e| e.stage).collect();
            assert_eq!(
                seen.as_slice(),
                &[
                    Stage::StreamingComplete,
                    Stage::Resolve,
                    Stage::Solve,
                    Stage::Rewind,
                    Stage::StreamingComplete,
                    Stage::Resolve,
                    Stage::Solve,
                    Stage::Simulate,
                    Stage::Gate,
                    Stage::Publish,
                    Stage::Finalize,
                ]
            );
            // The post-rewind cycle ran at the bumped generation.
            assert_eq!(harness.current, Some(Epoch::with_generation(201, 1)));
            assert!(!harness.rewinds.is_in_flight());
        }

        /// I6: a second rewind while one is in flight must FAIL FAST, never
        /// queue or silently overwrite.
        #[test]
        fn a_second_rewind_in_flight_fails_fast() {
            let mut tracker = RewindTracker::default();
            let first = Epoch::at(9).rewind();
            assert_eq!(tracker.open(first), Ok(first));
            assert!(tracker.is_in_flight());
            let second = Epoch::at(8).rewind();
            assert!(
                tracker.open(second).is_err(),
                "second rewind must fail fast"
            );
            tracker.settle();
            assert!(!tracker.is_in_flight());
            assert_eq!(tracker.open(second), Ok(second));
        }

        /// The order gate itself: driving a stage out of `legal_successors`
        /// order is a conformance failure, proving the harness asserts the
        /// ADR-041 stage order.
        #[test]
        fn out_of_order_stage_invocation_is_rejected() {
            let engine = NoopStubEngine::new();
            let mut harness = Harness::new(&engine);
            harness.current = Some(Epoch::at(1));
            assert!(harness.advance(Stage::StreamingComplete, None).is_ok());
            // Solve directly after streaming-complete: illegal (resolve is
            // required in between).
            assert!(matches!(
                harness.advance(Stage::Solve, None),
                Err(ConformanceError::StageOrder {
                    got: Stage::Solve,
                    ..
                })
            ));
        }

        /// Epoch monotonicity (I2/I3): a stream that moves the block
        /// backward WITHOUT a reorg (generation bump) is a conformance
        /// failure — rejected before any hook of the new cycle is driven.
        #[test]
        fn block_regression_without_a_rewind_is_rejected() {
            let engine = NoopStubEngine::new();
            let mut harness = Harness::new(&engine);
            let script = [
                StreamEvent::Epoch { block: 100 },
                StreamEvent::Epoch { block: 99 }, // regression, no reorg event
            ];
            assert!(matches!(
                harness.run_stream(&script),
                Err(ConformanceError::EpochRegressed { .. })
            ));
        }

        fn count(trace: &[TraceEntry], stage: Stage) -> usize {
            trace.iter().filter(|e| e.stage == stage).count()
        }
    }

    // ==================================================================
    // 7NFYQW landmine proof (guidance e): the NoopStubEngine conformance
    // harness driven by the REAL unified block stage machine
    // (`bot_core::stage_machine::StageMachine`) — the machine's
    // decisions select the cycles, the harness executes them on the
    // stub, and the conformance assertions (stage order, hook
    // completeness, epoch monotonicity, cutoff monotonicity) run over
    // the machine-driven trace end to end.
    // ==================================================================
    #[expect(clippy::expect_used)] // the conformance drives .expect() on cycles like the tests mod above
    mod machine_driven {
        use super::*;
        use crate::bot_core::stage_machine::{StageDecision, StageMachine, WatchdogPhase};

        fn meta(ts: u64) -> crate::bot_core::BlockMetadata {
            crate::bot_core::BlockMetadata {
                timestamp: ts,
                base_fee_per_gas: Some(ts),
                gas_used: 1,
                gas_limit: 2,
            }
        }

        fn count(trace: &[TraceEntry], stage: Stage) -> usize {
            trace.iter().filter(|e| e.stage == stage).count()
        }

        /// Feed the machine like the production driver: header + forward
        /// log + apply + settle; return the machine's settle decisions.
        fn settle_block(machine: &mut StageMachine, block: u64, now_ms: u64) -> Vec<StageDecision> {
            machine.on_header(block, meta(block * 1_000), now_ms);
            assert!(matches!(
                machine.on_log(block, false),
                crate::bot_core::LogDecision::DispatchForward
            ));
            machine.on_log_applied(block);
            machine.on_settle()
        }

        #[test]
        fn noop_stub_engine_conforms_under_the_unified_stage_machine() {
            let engine = NoopStubEngine::new();
            let mut harness = Harness::new(&engine);
            // The machine is the production decision surface; the pump
            // cursor starts at 99 (pre-cold-start).
            let mut machine = StageMachine::new(99, 0);

            // --- Block 100: quiesce + settle publish -> one full cycle.
            let d = settle_block(&mut machine, 100, 10);
            let StageDecision::Publish { open, .. } = d[0] else {
                unreachable!("the settled block must publish once: {d:?}")
            };
            assert_eq!(open, 100);
            assert_eq!(machine.stage(), Some(Stage::Publish));
            harness.run_epoch(open, None).expect("cycle 100 conforms");
            assert_eq!(harness.current, Some(machine.current_epoch()));

            // --- Block 101: the forward log tombstones 100 (Finalized
            // row) — the tombstone finalize window, then the 101 cycle.
            assert!(matches!(
                machine.on_log(101, false),
                crate::bot_core::LogDecision::TombstonePrevious(100)
            ));
            assert_eq!(machine.stage(), Some(Stage::Finalize));
            machine.on_log_applied(101);
            let d = machine.on_settle();
            let StageDecision::Publish { open, .. } = d[0] else {
                unreachable!("101 settles after its tombstone: {d:?}")
            };
            harness.run_epoch(open, None).expect("cycle 101 conforms");

            // --- Reorg: removed log rewinds the machine (I2 bump), pre-
            // rewind contexts fail fast (I3), stage row resets.
            let stale = machine.context_for(101, meta(101_000));
            assert!(matches!(
                machine.on_log(100, true),
                crate::bot_core::LogDecision::EnterReorg(100)
            ));
            assert_eq!(machine.rewind_seq(), 1);
            assert!(stale
                .epoch()
                .ensure_current(machine.current_epoch())
                .is_err());
            assert_eq!(machine.stage(), None, "rewind resets to Streaming");

            // The harness executes the Rewind row the machine just took:
            // the generation bumps exactly once (I2) and the rewind
            // tracker keeps single-in-flight semantics (I6).
            let current = harness.current.expect("epoch open at the rewind");
            let target = Epoch::with_generation(102, 1);
            harness
                .open_rewind(current, target)
                .expect("one rewind in flight, seq bumps once");
            harness
                .execute_rewind(target)
                .expect("rewind hook conforms");

            // The reorg window closes on the first forward; the fresh
            // cycle publishes at the bumped generation and conforms.
            assert!(matches!(
                machine.on_log(102, true),
                crate::bot_core::LogDecision::ContinueReorg
            ));
            assert!(matches!(
                machine.on_log(102, false),
                crate::bot_core::LogDecision::CloseReorg { new_head: 102 }
            ));
            machine.on_log_applied(102);
            let d = machine.on_settle();
            let StageDecision::Publish { open, .. } = d[0] else {
                unreachable!("post-rewind block settles: {d:?}")
            };
            // Sync the harness to the machine's bumped epoch (102 @ gen 1)
            // — the harness treats a cycle resuming exactly at the rewind
            // target as legal (I2/I6).
            let fresh = machine.current_epoch();
            assert_eq!(fresh, Epoch::with_generation(102, 1));
            harness.current = Some(fresh);
            harness.resumed_at = Some(fresh);
            harness
                .run_epoch(open, None)
                .expect("post-rewind cycle conforms");

            // Executable-spec walk over the machine-driven trace.
            harness
                .assert_conformance()
                .expect("all conformance invariants hold");
            let trace = harness.trace();
            assert_eq!(count(trace, Stage::StreamingComplete), 3);
            assert_eq!(count(trace, Stage::Publish), 3);
            assert_eq!(count(trace, Stage::Finalize), 3);
            // I7: cutoff stamps advance in epoch order (generation-dominant).
            let cut = harness.cutoff;
            assert_eq!(cut, Some(Epoch::with_generation(102, 1)));
            assert!(!harness.rewinds.is_in_flight());
        }

        /// The watchdog phase space (the dissolved `DrainerHealth`'s
        /// no-progress obligation) is representable off the REAL machine.
        #[test]
        fn watchdog_phase_space_is_representable_off_the_real_machine() {
            let mut machine = StageMachine::new(200, 1_000);
            machine.record_header(1_000);
            machine.record_log(1_000);
            assert_eq!(
                machine.watchdog_phase(1_050, 500, 300),
                WatchdogPhase::Healthy
            );
            // Headers stale (no header in 500ms): HeaderStale — the
            // machine's Recover decision covers it (see on_tick).
            assert_eq!(
                machine.watchdog_phase(1_700, 500, 300),
                WatchdogPhase::HeaderStale
            );
            machine.record_header(2_300);
            // Fresh headers, silent logs: LogsSilent — LogSilence's phase.
            assert_eq!(
                machine.watchdog_phase(2_400, 500, 300),
                WatchdogPhase::LogsSilent
            );
            assert!(machine.watchdog_phase(2_400, 500, 300).is_no_progress());
            // The on_tick decisions agree with the phase space.
            let d = machine.on_tick(2_400, 500, 300);
            assert!(d.iter().any(|x| matches!(x, StageDecision::LogSilence)));
        }
    }
}

// ======================================================================
// ergo 2KQZSC — RED pins for the candidate-2 stage-seam contract.
// Written against the TARGET contract; production code is NOT changed.
// ======================================================================
#[cfg(test)]
mod candidate2_seam_pins {
    use super::*;

    /// Pin 1 (RED: compile-fails until T2). The target splits the
    /// driver-facing pokes OFF `StageHandlers` onto a new required
    /// `PumpControl` trait beside this module, with exactly these seven
    /// methods and Epoch-typed cursor coordinates. `StageHandlers` keeps
    /// ONLY the eight stage hooks (`on_streaming_complete`, `on_resolve`,
    /// `on_solve`, `on_simulate`, `on_gate`, `on_publish`, `on_finalize`, `on_rewind`).
    /// This test names the trait + all seven poke signatures, so it cannot
    /// compile until `PumpControl` exists.
    #[test]
    fn candidate2_pumpcontrol_is_the_seven_poke_seam() {
        use crate::bot_core::PumpControl;
        let _has_dirty: fn(&dyn PumpControl) -> bool = PumpControl::has_dirty_paths;
        let _set_solved: fn(&dyn PumpControl, Epoch) = PumpControl::set_last_solved_block;
        let _set_anchor: fn(&dyn PumpControl, Epoch) = PumpControl::set_solve_anchor;
        let _record_logs: fn(&dyn PumpControl) = PumpControl::record_logs_this_block;
        let _last: fn(&dyn PumpControl) -> Option<Epoch> = PumpControl::last_processed_block;
        let _notify: fn(&dyn PumpControl, u64, &crate::bot_core::BlockMetadata) =
            PumpControl::notify_block;
        let _ended: fn(&dyn PumpControl) = PumpControl::on_pump_ended;

        // The eight hooks stay on `StageHandlers`; the pokes above must not.
        fn stage_hooks_only<T: StageHandlers + ?Sized>() {}
        stage_hooks_only::<dyn StageHandlers>();
    }

    /// Pin 6 (RED: compile-fails until T2; the `NoopStubEngine` half of the
    /// ADR-041 completeness proof). At the target the stub implements BOTH
    /// `StageHandlers` (eight hooks) AND `PumpControl` (seven pokes). The
    /// `FakeStageEngine` sibling pin lives in `block_pump.rs`.
    #[test]
    fn candidate2_noopstubengine_implements_both_traits() {
        fn assert_both<T: StageHandlers + crate::bot_core::PumpControl>() {}
        assert_both::<super::conformance::NoopStubEngine>();
    }
}
