//! `EngineStages` — the arb engine's `StageHandlers` implementation and the
//! Published-edge adapter for the delivery-to-Python channels (epic MROOY7,
//! task SZJUKL — the seam-retirement cutover).
//!
//! This is the ONE consumers-and-drivers surface that survived the seam
//! retirement: the dissolved `SolveCoordinator` / `DrainSink` / `Engine`
//! fan-out and the `EngineHandle` wrapper lock layer collapsed into this
//! type — the shared engine behind the `Arc<Mutex<ArbitrageEngine>>` the
//! Python wrapper co-owns, driven by the machine driver (the block pump)
//! through the stage hooks. There is no drain FIFO, no fan-out vec, no
//! `drain_lock`: solve/finalize/publish work runs INLINE in the pump task at
//! the machine's decision points, so the "drain-consistent cursor" problem
//! dissolves (single-writer by construction) and the stale-epoch drop the
//! old `DispatchOwner` FIFO needed is a cheap I3 check the driver runs at
//! each work site (see `block_pump::run_with_stream`).
//!
//! Channels survive ONLY at genuine asynchrony boundaries (SZJUKL):
//! - the **block clock** (delivery-to-Python header ticks) — an unbounded
//!   mpsc send per accepted header, never queued behind solver work;
//! - the **result batch** channel — the Published edge `on_publish` writes
//!   the debounced batch into; Python's consumer subscribes there as a
//!   sink (ADR-027 completion, B4GX7C lineage).
//!
//! The no-progress/strike obligations of the dissolved `DrainerHealth`
//! map onto the machine's `WatchdogPhase` (7NFYQW): header staleness =
//! a dead `newHeads` arm, log silence = a dead logs arm. A wedge that
//! stops the driver from executing stage work IS the pump stall the
//! machine's watchdogs already abort on — there is no separate drainer
//! task left to go silently dead.
//!
//! **Lock order:** engine `Mutex` alone (the driver runs between `BotState`
//! touches; the engine takes its own core read/write internally). The
//! block-clock send takes only its own mutex (never the engine).
//!
//! `latest_results` / `register_path` / the FFI surface keep their
//! StateLock-mediated core locking — this type adds NO lock layer.

use degenbot_core::op_error;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::bot_core::stage_handlers::StageHandlers;
use crate::bot_core::{
    stage_handlers::{
        AffectedPaths, Finalize, FinalizeOutcome, Gate, GateOutcome, Publish, PublishOutcome,
        QuiesceOutcome, Resolve, Simulate, SimulateOutcome, Solve, SolveOutcome, StageError,
    },
    BlockMetadata, Epoch, EpochDelta, Rewind, RewindOutcome,
};
use degenbot_core::block_clock_pipe::{BlockClockPipe, BlockNotification};

use degenbot_solvers::affected_keys::AffectedKey;

use super::ArbitrageEngine;

/// The arb engine's stage surface: the shared engine + the touched-pool
/// ledger it solves from + the delivered-to-Python block clock.
pub struct EngineStages {
    /// The shared engine state (the same `Arc` `PyArbitrageEngine.engine`
    /// holds). The stage hooks lock per call — the SRQEK5 detached-solve
    /// posture keeps the steady-state hold at enqueue length (µs).
    engine: Arc<Mutex<ArbitrageEngine>>,
    /// The epoch ledger the hooks take keys from (wired to
    /// `Bot::active_delta` so log application records into the SAME ledger
    /// `on_resolve` consumes — LXDY4C shared dirty tracking).
    delta: RwLock<Arc<EpochDelta>>,
    /// The block-clock pipe (delivery-to-Python at the async boundary).
    /// Header ticks never touch the engine (a chain fact, not engine
    /// business — B2/ADR-027 lineage; the pipe moved here from the
    /// dissolved coordinator).
    block_clock: Mutex<BlockClockPipe>,
}

impl EngineStages {
    /// Construct over a strong clone of the shared engine handle.
    #[must_use]
    pub fn new(engine: Arc<Mutex<ArbitrageEngine>>) -> Self {
        Self {
            engine,
            delta: RwLock::new(Arc::new(EpochDelta::new(0u64))),
            block_clock: Mutex::new(BlockClockPipe::default()),
        }
    }

    /// Hand the stage surface the shared epoch ledger (the wiring layer
    /// passes `Bot::active_delta`).
    ///
    /// KJWIK5: this is also the installer for the engine's deferred-path
    /// re-record hook — the ledger carry for `paths.deferred_future_price`
    /// deferrals. The engine's dispatch maps a deferred path's pid to its
    /// hop-pool keys and calls the hook with the cycle's solve block; the
    /// closure re-records them into THIS shared ledger, so the next draw
    /// re-includes the path through the same freshness ordering, admission
    /// budget, and retention window (one deferral concept). This is the ONE
    /// engine access to the ledger (engine-side ownership was deliberately
    /// avoided — LXDY4C); lock order stays engine mutex outer, ledger mutex
    /// inner, matching `on_resolve`.
    pub fn set_delta(&self, delta: Arc<EpochDelta>) {
        *self.delta.write() = Arc::clone(&delta);
        let ledger = delta;
        self.engine
            .lock()
            .set_deferred_re_record(Arc::new(move |keys, block| {
                for &key in keys {
                    ledger.record(key, block);
                }
            }));
    }

    /// Attach the block-clock channel sender (the wiring layer creates the
    /// channel pair; the Python-facing receiver lives on `PumpState`).
    /// The pipe mutex is a non-poisoning `parking_lot` — a nanosecond send,
    /// never queued behind solver work (B2).
    pub fn set_block_channel(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::bot_core::BlockNotification>,
    ) {
        self.block_clock.lock().set_channel(tx);
    }

    /// Test probe: clone of the actively-shared delta ledger.
    #[cfg(test)]
    #[must_use]
    pub fn delta_for_test(&self) -> Arc<EpochDelta> {
        Arc::clone(&self.delta.read())
    }

    /// The engine's solve cycle — the behavior port of the dissolved
    /// `EngineHandle::solve_dirty` hold/spans/sidecar logic, verbatim.
    /// Public: the solve-shaped tests and the registration eager-solve
    /// path drive it directly; the driver arrives via `StageHandlers::on_solve`.
    pub fn solve_dirty(&self, affected: &[AffectedKey], block: u64, metadata: &BlockMetadata) {
        self.run_solve_cycle(affected, block, metadata);
    }

    /// The drained-cursor read (the engine's own `last_processed_block`).
    #[must_use]
    pub fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    /// Flush a debounced result batch (the Published-edge delivery).
    pub fn send_result_batch(&self, metadata: &BlockMetadata) {
        self.engine.lock().send_result_batch(metadata);
    }

    /// The tombstone boundary catch (advance + terminal publish).
    pub fn finalize_block(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().finalize_block(block, metadata);
    }

    /// Mark `block` solved (engine-owned bookkeeping since LEZJAS).
    pub fn set_last_solved_block(&self, block: u64) {
        self.engine.lock().set_last_solved_block(block);
    }

    /// Seed the cold-start `results_block` anchor (resume boundary).
    pub fn set_solve_anchor(&self, block: u64) {
        self.engine.lock().set_solve_anchor(block);
    }

    /// Record a forward-log apply this block (LEZJAS bookkeeping).
    pub fn record_logs_this_block(&self) {
        self.engine.lock().record_logs_this_block();
    }

    /// Pump death: close the block clock + the delivery channels.
    pub fn on_pump_ended(&self) {
        self.block_clock.lock().close();
        self.engine.lock().on_pump_ended();
    }

    fn run_solve_cycle(
        &self,
        affected: &[degenbot_solvers::affected_keys::AffectedKey],
        block: u64,
        metadata: &BlockMetadata,
    ) {
        // P5FEOI / T0 / K4ETHF span-gate lineage preserved verbatim from the
        // dissolved wrapper: one Jaeger node for dirty solves, none for the
        // ~2µs empty pass, gate + work under ONE mutex acquisition.
        let mut engine =
            hotpath::measure_block!("EngineStages::solve.probe_lock", self.engine.lock());
        // WFF6MM: this path spawns the merge sidecar AFTER `solve_dirty`
        // returns (below), and the machine's merge Receiver is take-once —
        // a direct-call inline drain (the synchronous unit-test harness)
        // would steal it. Disable the inline drain for every EngineStages-
        // driven engine; the sidecar owns the pipe here.
        #[cfg(test)]
        engine.set_sync_merge_for_test(false);
        if affected.is_empty() {
            // Kept for inner bookkeeping parity (last_processed_block et al);
            // provably cannot consume dirt under this continuous hold.
            engine.solve_dirty(block, metadata, affected);
            drop(engine);
            self.spawn_detached_sidecar_if_pending();
            return;
        }
        // REMED1 T2: the streaming (drain) entry tags its cycles.
        engine.set_solve_entry("drain");
        let span = tracing::info_span!(
            "degenbot.arb.solve",
            block.number = block,
            cycle.solve_block = tracing::field::Empty,
            // Cold-start trace: the dispatch arm ("detached" |
            // "skipped_empty" | "shed"), recorded at the machine's begin_cycle
            // verdict (or the admission shed) in solver_dispatch.
            cycle.arm = tracing::field::Empty,
        );
        // ZZS6CG: exact-match reparent onto this block's published pump
        // span. The work now runs INLINE in the driver (no drainer task), so
        // the ambient span is already the pump's block span; the published-
        // parent attach keeps the block-boundary exactness.
        crate::telemetry::attach_published_parent_exact(&span, block);
        let _guard = span.enter();
        // T3: solve duration + registered-path gauge (dirty solves only).
        let solve_start = std::time::Instant::now();
        {
            if let Some(p) = crate::instruments::pipeline() {
                p.set_registered_paths(u64::try_from(engine.path_count()).unwrap_or(u64::MAX));
            }
            hotpath::gauge!("engine_registered_paths").set(f64::from(
                u32::try_from(engine.path_count()).unwrap_or(u32::MAX),
            ));
            // T3 (epic BXZBWY): the solve cycle must not pin a shared
            // pump-runtime worker while it runs. 2UVG3E seam #4: under the
            // detached stance the engine Mutex hold collapses to enqueue end
            // (µs); the in-cycle arm is the backpressure safety valve only.
            let hold_start = std::time::Instant::now();
            if is_multi_thread_runtime() {
                tokio::task::block_in_place(|| engine.solve_dirty(block, metadata, affected));
            } else {
                engine.solve_dirty(block, metadata, affected);
            }
            // KNEUQX: surface the cycle's anchored block on the span.
            span.record("cycle.solve_block", engine.results_block());
            // Cold-start trace: the arm the dispatch latched THIS cycle on
            // (the span field is unreadable here) attributes the hold sample.
            let cycle_arm = engine.cycle_arm();
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_mutex_hold_duration(hold_start.elapsed().as_secs_f64(), cycle_arm);
            }
            // SRQEK5 (WV62TX): spawn the detached merge sidecar at the FIRST
            // detached enqueue (rx take + spawn atomic under the held guard).
            // P37YJG: THE ONE spawn — the machine owns the census register +
            // named thread + loud abort; this site only takes the parked rx.
            if let Some(merge_rx) = engine.detached_cycle.take_merge_rx() {
                super::detached_cycle::spawn_merge_sidecar(&self.engine, merge_rx);
            }
        }
        if let Some(p) = crate::instruments::pipeline() {
            p.observe_solve_duration(solve_start.elapsed().as_secs_f64(), engine.cycle_arm());
            p.count_solves_executed();
        }
    }

    /// SRQEK5 (WV62TX): if the empty-affected solve path took the parked
    /// Receiver tradeoff, the sidecar spawn happens here instead.
    /// P37YJG: THE ONE spawn — the machine owns it (the take-once rides the
    /// machine; the census/thread/abort body is `spawn_merge_sidecar`).
    fn spawn_detached_sidecar_if_pending(&self) {
        let Some(merge_rx) = self.engine.lock().detached_cycle.take_merge_rx() else {
            return;
        };
        super::detached_cycle::spawn_merge_sidecar(&self.engine, merge_rx);
    }
}

// P37YJG: the sidecar's thread name + census row + the ONE spawn moved
// into the machine — `detached_cycle::{merge_sidecar_thread_name,
// merge_sidecar_census_entry, spawn_merge_sidecar}` (byte-identical
// naming and census row).

/// Is the caller inside an ambient multi-thread tokio runtime? `block_in_place`
/// is only valid there; a current-thread runtime or no runtime runs inline.
fn is_multi_thread_runtime() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

impl StageHandlers for EngineStages {
    /// Quiesced row: the machine classifies completeness (tombstone vs
    /// settle) — the engine takes no action; the solve cycle the quiesce
    /// arms is the driver's drained-settle gate below.
    fn on_streaming_complete(
        &self,
        _work: &crate::bot_core::stage_handlers::StreamingComplete<'_>,
    ) -> Result<QuiesceOutcome, StageError> {
        Ok(QuiesceOutcome {
            verdict: crate::bot_core::stage_handlers::QuiesceVerdict::Settled,
        })
    }

    /// Resolved row: consume the epoch ledger's touched keys ONCE (the
    /// LXDY4C take preserves the retired `DirtySets::take_all` semantics;
    /// keys recorded while this drain runs land in the NEXT cycle).
    ///
    /// QTZGFL: under the construction-stamped admission stance this is a
    /// capacity-modulated DRAW — `budget = max(0, target − in-flight)` KEYS,
    /// freshest-first, the overflow RETAINED for a later cycle (carry). A zero
    /// budget draws nothing; the engine's solve cycle then sheds. The same
    /// site prunes carried keys older than `head − W` (the retention window)
    /// and counts the expiry. Lock order: the engine mutex is taken first and
    /// the ledger mutex inside `expire_older_than`/`draw_freshest` is the
    /// inner lock — never the reverse.
    ///
    /// QTZGFL: THIS is the SINGLE consumption decision (F3). The zero-budget
    /// verdict is stashed on the engine for the same cycle's dispatch — the
    /// dispatch consumes and clears it under the engine mutex and never
    /// re-reads the live gauge. The stage machine drives Resolved -> Solved
    /// sequentially on the driver thread, so no other draw can interleave
    /// between the stash and the read.
    fn on_resolve(&self, work: &Resolve<'_>) -> Result<AffectedPaths, StageError> {
        let mut engine = self.engine.lock();
        let admission = engine
            .admission_budget_keys()
            .map(|budget| (budget, engine.admission_retention_blocks));
        let affected = if let Some((budget, retention)) = admission {
            let cutoff = work.ctx.block().saturating_sub(retention);
            let expired = work.delta.expire_older_than(cutoff);
            engine.detached_cycle.note_leads_expired(expired);
            // Stash the draw-time verdict BEFORE drawing: the drawn keys
            // leave the ledger, so the dispatch must honor THIS cycle's
            // decision, not a fresh gauge read.
            engine.admission_draw_zero = budget == 0;
            work.delta.draw_freshest(budget)
        } else {
            engine.admission_draw_zero = false;
            work.delta.take_keys()
        };
        drop(engine);
        Ok(AffectedPaths(affected))
    }

    /// Solved row: the engine's solve cycle over the affected keys. The
    /// in-process simulation (ADR-019) and the gate (ADR-040) run INSIDE
    /// this engine cycle (`solve_dirty` → solver dispatch + inline sim);
    /// results stream on the delivery channel, not on the hook return.
    fn on_solve(&self, work: &Solve) -> Result<SolveOutcome, StageError> {
        self.run_solve_cycle(&work.paths.0, work.ctx.block(), work.ctx.metadata());
        Ok(SolveOutcome::default())
    }

    /// Simulated row: in-process revm simulation ran inside the Solved
    /// cycle (the engine's sole-executor posture is a property of the
    /// cycle, not a separate pass). No engine action.
    fn on_simulate(&self, _work: &Simulate) -> Result<SimulateOutcome, StageError> {
        Ok(SimulateOutcome::default())
    }

    /// Gated row: the deliver/reject verdicts are produced inside the
    /// Solved cycle's gate (ADR-040). No engine action.
    fn on_gate(&self, _work: &Gate) -> Result<GateOutcome, StageError> {
        Ok(GateOutcome::default())
    }

    /// Published row: the delivery-to-Python edge — flush the debounced
    /// result batch (delivery/submission/Python are sinks at THIS edge,
    /// not seams in front of the engine).
    fn on_publish(&self, work: &Publish) -> Result<PublishOutcome, StageError> {
        self.engine.lock().send_result_batch(work.ctx.metadata());
        Ok(PublishOutcome::default())
    }

    /// Finalized row: the boundary catch — advance + terminal publish, no
    /// solve cycle (PWPPAZ T1).
    fn on_finalize(&self, work: &Finalize) -> Result<FinalizeOutcome, StageError> {
        self.engine
            .lock()
            .finalize_block(work.ctx.block(), work.ctx.metadata());
        Ok(FinalizeOutcome {
            cutoff: work.ctx.epoch(),
        })
    }

    /// `Rewind` row: the machine owns the unwind (epoch seq bump, stale
    /// contexts fail fast); pool restoration is the event-driven
    /// `ReorgCoordinator` per-log path, not a stage hook. Echoes the target.
    fn on_rewind(&self, work: &Rewind) -> Result<RewindOutcome, StageError> {
        Ok(RewindOutcome {
            restored_to: work.to_epoch,
        })
    }

    fn has_dirty_paths(&self) -> bool {
        !self.delta.read().is_empty()
    }

    fn set_last_solved_block(&self, solved: Epoch) {
        self.engine.lock().set_last_solved_block(solved.block());
    }

    fn set_solve_anchor(&self, anchor: Epoch) {
        self.engine.lock().set_solve_anchor(anchor.block());
    }

    fn record_logs_this_block(&self) {
        self.engine.lock().record_logs_this_block();
    }

    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
        // Direct, non-FIFO dispatch: one send per accepted header, never
        // queued behind solver work (B2). NOT taking the engine lock.
        self.block_clock.lock().notify(BlockNotification {
            number: block,
            timestamp: metadata.timestamp,
            base_fee_per_gas: metadata.base_fee_per_gas,
            gas_used: metadata.gas_used,
            gas_limit: metadata.gas_limit,
        });
    }

    fn on_pump_ended(&self) {
        op_error!(domain = solver, "EngineStages: pump ended - closing the block-clock pipe + engine delivery channels; the Python block/result streams now end so the bot fails loudly"
        );
        self.block_clock.lock().close();
        self.engine.lock().on_pump_ended();
    }
}

#[cfg(test)]
mod fleet_stance_tests {
    //! BCA77G: the merge sidecar hosted as the fleet `Merge` role. LW-T9:
    //! the fleet.stance flip matrix is retired — ONE posture survives.

    // P37YJG: the naming/census fns moved into the machine; the pins
    // (byte-identical naming + census row) stay right here.
    use crate::arb_engine::detached_cycle::{
        merge_sidecar_census_entry, merge_sidecar_thread_name,
    };

    /// Under `fleet.stance=fleet` the sidecar runs as the pinned `Merge`
    /// role: the role's greppable thread-name pattern and the fleet merge
    /// census row (exactly one seat).
    #[test]
    fn fleet_stance_hosts_the_sidecar_as_the_merge_role() {
        let name = merge_sidecar_thread_name();
        assert_eq!(
            name, "work-fleet-merge-1",
            "role.thread_name() with the seat index"
        );
        let row = merge_sidecar_census_entry();
        assert_eq!(row.resource, "fleet_merge_slots");
        assert_eq!(row.thread_name, "work-fleet-merge-{n}");
        assert_eq!(row.count, 1);
    }

    /// LW-T9: there IS no legacy posture — every construction hosts the
    /// sidecar as the fleet `Merge` role, byte-identical to the fleet arm
    /// (RED before the cutover: the false-stance branch kept the
    /// historical "arb-detached-merge" identity).
    #[test]
    fn every_construction_hosts_the_sidecar_as_the_merge_role() {
        let name = merge_sidecar_thread_name();
        assert_eq!(
            name, "work-fleet-merge-1",
            "the historical legacy identity is retired: every sidecar is \
             the pinned Merge seat"
        );
        let row = merge_sidecar_census_entry();
        assert_eq!(row.resource, "fleet_merge_slots");
        assert_eq!(row.thread_name, "work-fleet-merge-{n}");
        assert_eq!(row.count, 1);
    }
}
