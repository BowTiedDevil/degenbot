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

use parking_lot::Mutex;

use crate::bot_core::stage_handlers::StageHandlers;
use crate::bot_core::{
    stage_handlers::{
        AffectedPaths, Finalize, FinalizeOutcome, Gate, GateOutcome, Publish, PublishOutcome,
        QuiesceOutcome, Resolve, Simulate, SimulateOutcome, Solve, SolveOutcome, StageError,
    },
    BlockMetadata, Epoch, EpochDelta, PumpControl, Rewind, RewindOutcome,
};
use degenbot_core::block_clock_pipe::{BlockClockPipe, BlockNotification};

use super::solve_cycle::CycleOutcome;
use super::ArbitrageEngine;
use super::EngineRetune;

/// THE one arm-attribution wiring site (cold-start trace): the cycle span is
/// tagged with `cycle.arm` (`detached` | `skipped_empty` | `shed`; `unset`
/// before any cycle). Pipeline-free by design: a consumer without the meter
/// installed is a no-op (pure-Rust/test seams).
///
/// ADR-045 T5: the caller drives it with `CycleOutcome::arm_label()` — the
/// cycle's duration/Mutex hold are observed a frame up, in `EngineStages`,
/// after `solve_dirty` returns, so the OUTCOME (not a post-hoc engine stash)
/// is the byte-stable source of the label.
#[must_use = "returns the label it recorded; callers may name the cycle arm with it"]
pub(crate) fn record_cycle_arm_telemetry(span: &tracing::Span, arm: &'static str) -> &'static str {
    span.record("cycle.arm", arm);
    // Handed back for the caller's per-cycle latch (see the doc above).
    arm
}

/// The arb engine's stage surface: the shared engine + the touched-pool
/// ledger it solves from + the delivered-to-Python block clock.
pub struct EngineStages {
    /// The shared engine state (the same `Arc` `PyArbitrageEngine.engine`
    /// holds). The stage hooks lock per call — the SRQEK5 detached-solve
    /// posture keeps the steady-state hold at enqueue length (µs).
    engine: Arc<Mutex<ArbitrageEngine>>,
    /// The epoch ledger the hooks take keys from — the ONE Bot-owned
    /// ledger (`Bot::active_delta`) injected at construction, so log
    /// application records into the SAME ledger `on_resolve` consumes
    /// (LXDY4C shared dirty tracking). Identity is structural: there is no
    /// swap surface and no second handle.
    delta: Arc<EpochDelta>,
    /// The block-clock pipe (delivery-to-Python at the async boundary).
    /// Header ticks never touch the engine (a chain fact, not engine
    /// business — B2/ADR-027 lineage; the pipe moved here from the
    /// dissolved coordinator).
    block_clock: Mutex<BlockClockPipe>,
}

impl EngineStages {
    /// Construct over a strong clone of the shared engine handle and the
    /// ONE Bot-owned epoch ledger (the wiring layer passes
    /// `Bot::active_delta`).
    ///
    /// KJWIK5: construction is also the installer for the engine's
    /// deferred-path re-record hook — the ledger carry for
    /// `paths.deferred_future_price` deferrals. The engine's dispatch maps a
    /// deferred path's pid to its hop-pool keys and calls the hook with the
    /// cycle's solve block; the closure re-records them into THIS shared
    /// ledger, so the next draw re-includes the path through the same
    /// freshness ordering, admission budget, and retention window (one
    /// deferral concept). This is the ONE engine access to the ledger
    /// (engine-side ownership was deliberately avoided — LXDY4C); lock order
    /// stays engine mutex outer, ledger mutex inner, matching `on_resolve`.
    #[must_use]
    pub fn new(engine: Arc<Mutex<ArbitrageEngine>>, delta: Arc<EpochDelta>) -> Self {
        {
            let ledger = Arc::clone(&delta);
            engine
                .lock()
                .set_deferred_re_record(Arc::new(move |keys, block| {
                    for &key in keys {
                        ledger.record(key, block);
                    }
                }));
        }
        Self {
            engine,
            delta,
            block_clock: Mutex::new(BlockClockPipe::default()),
        }
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

    /// Apply an operator [`EngineRetune`] to the live engine — the engine's
    /// twin of the fleet's centralized posture feeder + wake (43121b9). This
    /// is the ONE runtime re-parameterization entry: the stage surface owns
    /// the lock, the engine applies every knob under it, and every reader
    /// consults the live values on the next cycle (the engine parks no host,
    /// so there is no separate wake edge to fire).
    pub fn apply_retune(&self, retune: &EngineRetune) {
        self.engine.lock().apply_retune(retune);
    }

    /// The engine's solve cycle — the behavior port of the dissolved
    /// `EngineHandle::solve_dirty` hold/spans/sidecar logic, verbatim.
    ///
    /// ADR-046 / ZE67AE: this is the **cycle surface** on `EngineStages`
    /// (the solve entry that deliberately bypasses pump semantics). The
    /// driver arrives via `StageHandlers::on_solve`; the stage-span and
    /// detached-sidecar unit-test harnesses drive it directly. The
    /// engine-level `ArbitrageEngine::solve_dirty` remains the internal
    /// cycle fn. The eight inherent twins were hard-cut (no shims).
    pub(crate) fn run_solve_cycle(
        &self,
        affected: &[degenbot_solvers::affected_keys::AffectedKey],
        block: u64,
        metadata: &BlockMetadata,
    ) -> CycleOutcome {
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
        engine.cycle.set_sync_merge_for_test(false);
        if affected.is_empty() {
            // Kept for inner bookkeeping parity (last_processed_block et al);
            // provably cannot consume dirt under this continuous hold.
            let outcome = engine.solve_dirty(block, metadata, affected);
            drop(engine);
            self.spawn_detached_sidecar_if_pending();
            return outcome;
        }
        let span = tracing::info_span!(
            "degenbot.arb.solve",
            block.number = block,
            cycle.solve_block = tracing::field::Empty,
            // Cold-start trace: the dispatch arm ("detached" |
            // "skipped_empty" | "shed"), recorded at the machine's begin_cycle
            // verdict (or the admission shed) in the solve cycle.
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
        // ADR-045 T5: the typed cycle outcome flows out of the engine and
        // attributes the span + the duration/hold histograms below.
        let cycle_outcome;
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
            cycle_outcome = if is_multi_thread_runtime() {
                tokio::task::block_in_place(|| engine.solve_dirty(block, metadata, affected))
            } else {
                engine.solve_dirty(block, metadata, affected)
            };
            // KNEUQX: surface the cycle's anchored block on the span.
            span.record("cycle.solve_block", engine.results_block());
            // Cold-start trace (ADR-045 T5): attribute the cycle arm from the
            // typed OUTCOME, never a post-hoc engine stash.
            let _ = record_cycle_arm_telemetry(&span, cycle_outcome.arm_label());
            // ADR-045 T5: keep the outcome's full typed record live at the
            // consumer seam (the census is the same resolve counts the cycle
            // already emitted; the solved-block is the cycle's anchor).
            let _ = cycle_outcome.solved_block();
            let _ = cycle_outcome.census;
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_mutex_hold_duration(
                    hold_start.elapsed().as_secs_f64(),
                    cycle_outcome.arm_label(),
                );
            }
            // SRQEK5 (WV62TX): spawn the detached merge sidecar at the FIRST
            // detached enqueue (rx take + spawn atomic under the held guard).
            // P37YJG: THE ONE spawn — the machine owns the census register +
            // named thread + loud abort; this site only takes the parked rx.
            if let Some(merge_rx) = engine.cycle.detached_cycle.take_merge_rx() {
                super::detached_cycle::spawn_merge_sidecar(&self.engine, merge_rx);
            }
        }
        if let Some(p) = crate::instruments::pipeline() {
            p.observe_solve_duration(
                solve_start.elapsed().as_secs_f64(),
                cycle_outcome.arm_label(),
            );
            p.count_solves_executed();
        }
        cycle_outcome
    }

    /// SRQEK5 (WV62TX): if the empty-affected solve path took the parked
    /// Receiver tradeoff, the sidecar spawn happens here instead.
    /// P37YJG: THE ONE spawn — the machine owns it (the take-once rides the
    /// machine; the census/thread/abort body is `spawn_merge_sidecar`).
    fn spawn_detached_sidecar_if_pending(&self) {
        let Some(merge_rx) = self.engine.lock().cycle.detached_cycle.take_merge_rx() else {
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
        // ADR-045 T4: the admission draw is the cycle's `draw` (engine mutex
        // held, ledger mutex inner - order unchanged).
        let affected = engine.cycle.draw(work.delta, work.ctx.block());
        drop(engine);
        Ok(AffectedPaths(affected))
    }

    /// Solved row: the engine's solve cycle over the affected keys. The
    /// in-process simulation (ADR-019) and the gate (ADR-040) run INSIDE
    /// this engine cycle (`solve_dirty` → solver dispatch + inline sim);
    /// results stream on the delivery channel, not on the hook return.
    fn on_solve(&self, work: &Solve) -> Result<SolveOutcome, StageError> {
        // The cycle's typed outcome carries the anchor epoch it solved; pack
        // that cursor fact onto the Solved row's outcome so the driver can
        // derive the engine cursor from the product rather than re-poking
        // the seam after the hook returns.
        let outcome = self.run_solve_cycle(&work.paths.0, work.ctx.block(), work.ctx.metadata());
        Ok(SolveOutcome {
            candidates: Vec::new(),
            solved: Epoch::at(outcome.solved_block()),
        })
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
}

/// ADR-046: the driver-facing control seam, split OFF `StageHandlers` so the
/// stage trait carries only the eight pure hooks. `EngineStages` implements
/// both; the pump injects both Arcs at construction.
impl PumpControl for EngineStages {
    fn has_dirty_paths(&self) -> bool {
        !self.delta.is_empty()
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

    fn last_processed_block(&self) -> Option<Epoch> {
        self.engine.lock().last_processed_block().map(Epoch::at)
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

// ======================================================================
// ergo 2KQZSC — RED pin for the candidate-2 stage-seam contract.
// Written against the TARGET contract; production code is NOT changed.
// ======================================================================
#[cfg(test)]
mod candidate2_seam_pins {
    use std::sync::Arc;

    /// Pin 2 (GREEN after T2; extended at ZE67AE so it cannot quietly rot).
    ///
    /// The eight `EngineStages` inherent twins are killed HARD:
    /// `solve_dirty`, `last_processed_block`, `send_result_batch`,
    /// `finalize_block`, `set_last_solved_block(u64)`, `set_solve_anchor(u64)`,
    /// `record_logs_this_block`, `on_pump_ended`. The seven pokes survive
    /// only as the `PumpControl` impl (Epoch-typed cursors) and
    /// `solve_dirty` is gone (the cycle surface is `run_solve_cycle`).
    ///
    /// Two halves:
    /// 1. binding `EngineStages` to `PumpControl` proves the trait impl.
    /// 2. `NoInherentTwinProbe` names all eight twin methods with a token
    ///    argument. Rust prefers an inherent method over any trait method
    ///    during method resolution, so if ANY twin reappears as an inherent
    ///    `EngineStages` method the probe call below resolves to it and
    ///    fails to compile (arity/type mismatch) — the pin is a
    ///    compile-time inherent-absence check, not a textual one.
    #[test]
    fn candidate2_enginestages_has_no_inherent_poke_twins() {
        // Absence probe: each probe method shadows nothing while the twin is
        // gone; an inherent re-introduction would shadow the probe. Defined
        // ahead of the statements to keep the module clippy-clean.
        struct TwinProbeToken;
        trait NoInherentTwinProbe {
            fn solve_dirty(&self, _t: TwinProbeToken);
            fn last_processed_block(&self, _t: TwinProbeToken);
            fn send_result_batch(&self, _t: TwinProbeToken);
            fn finalize_block(&self, _t: TwinProbeToken);
            fn set_last_solved_block(&self, _t: TwinProbeToken);
            fn set_solve_anchor(&self, _t: TwinProbeToken);
            fn record_logs_this_block(&self, _t: TwinProbeToken);
            fn on_pump_ended(&self, _t: TwinProbeToken);
            // Full driver surface (ergo 2NLZE3): the eight `StageHandlers`
            // hooks + the remaining `PumpControl` methods are TRAIT methods
            // on the stage surface — an inherent `EngineStages` twin would
            // shadow them in the method calls below and fail to compile.
            fn on_streaming_complete(&self, _t: TwinProbeToken);
            fn on_resolve(&self, _t: TwinProbeToken);
            fn on_solve(&self, _t: TwinProbeToken);
            fn on_simulate(&self, _t: TwinProbeToken);
            fn on_gate(&self, _t: TwinProbeToken);
            fn on_publish(&self, _t: TwinProbeToken);
            fn on_finalize(&self, _t: TwinProbeToken);
            fn on_rewind(&self, _t: TwinProbeToken);
            fn has_dirty_paths(&self, _t: TwinProbeToken);
            fn notify_block(&self, _t: TwinProbeToken);
        }
        impl NoInherentTwinProbe for super::EngineStages {
            fn solve_dirty(&self, _t: TwinProbeToken) {}
            fn last_processed_block(&self, _t: TwinProbeToken) {}
            fn send_result_batch(&self, _t: TwinProbeToken) {}
            fn finalize_block(&self, _t: TwinProbeToken) {}
            fn set_last_solved_block(&self, _t: TwinProbeToken) {}
            fn set_solve_anchor(&self, _t: TwinProbeToken) {}
            fn record_logs_this_block(&self, _t: TwinProbeToken) {}
            fn on_pump_ended(&self, _t: TwinProbeToken) {}
            fn on_streaming_complete(&self, _t: TwinProbeToken) {}
            fn on_resolve(&self, _t: TwinProbeToken) {}
            fn on_solve(&self, _t: TwinProbeToken) {}
            fn on_simulate(&self, _t: TwinProbeToken) {}
            fn on_gate(&self, _t: TwinProbeToken) {}
            fn on_publish(&self, _t: TwinProbeToken) {}
            fn on_finalize(&self, _t: TwinProbeToken) {}
            fn on_rewind(&self, _t: TwinProbeToken) {}
            fn has_dirty_paths(&self, _t: TwinProbeToken) {}
            fn notify_block(&self, _t: TwinProbeToken) {}
        }
        fn is_pump_control<T: crate::bot_core::PumpControl>() {}
        is_pump_control::<super::EngineStages>();

        let stages = super::EngineStages::new(
            Arc::new(parking_lot::Mutex::new(super::ArbitrageEngine::new())),
            Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        stages.solve_dirty(TwinProbeToken);
        stages.last_processed_block(TwinProbeToken);
        stages.send_result_batch(TwinProbeToken);
        stages.finalize_block(TwinProbeToken);
        stages.set_last_solved_block(TwinProbeToken);
        stages.set_solve_anchor(TwinProbeToken);
        stages.record_logs_this_block(TwinProbeToken);
        stages.on_pump_ended(TwinProbeToken);
        stages.on_streaming_complete(TwinProbeToken);
        stages.on_resolve(TwinProbeToken);
        stages.on_solve(TwinProbeToken);
        stages.on_simulate(TwinProbeToken);
        stages.on_gate(TwinProbeToken);
        stages.on_publish(TwinProbeToken);
        stages.on_finalize(TwinProbeToken);
        stages.on_rewind(TwinProbeToken);
        stages.has_dirty_paths(TwinProbeToken);
        stages.notify_block(TwinProbeToken);
    }

    /// Driver-surface twin probe (ergo 2NLZE3 T1, epic 5TBT7L).
    ///
    /// The candidate-2 end state makes the `EngineStages` stage surface the
    /// ONE driver interface: the pump drives `run_solve_cycle`,
    /// `set_block_channel`, and the `StageHandlers` hooks, and
    /// `ArbitrageEngine` recedes to `pub(crate)` machinery behind it. This
    /// compile probe pins the surfaces themselves: none of the driver-surface
    /// method names may exist as INHERENT `ArbitrageEngine` twins. If one
    /// reappears, method resolution prefers the inherent method over this
    /// probe's token-taking method at the call sites below and the
    /// arity/type mismatch fails the build.
    ///
    /// The probed names are the members of the driver surface that are
    /// poke-free on the engine TODAY (the engine's real surface is
    /// `solve_dirty`, the lifecycle/delivery setters, and the machine
    /// pokes), so the pin is green now and turns red exactly when a driver
    /// name leaks onto the engine.
    #[test]
    fn candidate2_driver_surface_stays_off_the_engine() {
        struct DriverTwinProbeToken;
        trait NoInherentDriverTwinProbe {
            fn run_solve_cycle(&self, _t: DriverTwinProbeToken);
            fn set_block_channel(&self, _t: DriverTwinProbeToken);
            fn on_streaming_complete(&self, _t: DriverTwinProbeToken);
            fn on_resolve(&self, _t: DriverTwinProbeToken);
            fn on_solve(&self, _t: DriverTwinProbeToken);
            fn on_simulate(&self, _t: DriverTwinProbeToken);
            fn on_gate(&self, _t: DriverTwinProbeToken);
            fn on_publish(&self, _t: DriverTwinProbeToken);
            fn on_finalize(&self, _t: DriverTwinProbeToken);
            fn on_rewind(&self, _t: DriverTwinProbeToken);
            fn has_dirty_paths(&self, _t: DriverTwinProbeToken);
            fn notify_block(&self, _t: DriverTwinProbeToken);
        }
        impl NoInherentDriverTwinProbe for super::ArbitrageEngine {
            fn run_solve_cycle(&self, _t: DriverTwinProbeToken) {}
            fn set_block_channel(&self, _t: DriverTwinProbeToken) {}
            fn on_streaming_complete(&self, _t: DriverTwinProbeToken) {}
            fn on_resolve(&self, _t: DriverTwinProbeToken) {}
            fn on_solve(&self, _t: DriverTwinProbeToken) {}
            fn on_simulate(&self, _t: DriverTwinProbeToken) {}
            fn on_gate(&self, _t: DriverTwinProbeToken) {}
            fn on_publish(&self, _t: DriverTwinProbeToken) {}
            fn on_finalize(&self, _t: DriverTwinProbeToken) {}
            fn on_rewind(&self, _t: DriverTwinProbeToken) {}
            fn has_dirty_paths(&self, _t: DriverTwinProbeToken) {}
            fn notify_block(&self, _t: DriverTwinProbeToken) {}
        }
        let engine = super::ArbitrageEngine::new();
        engine.run_solve_cycle(DriverTwinProbeToken);
        engine.set_block_channel(DriverTwinProbeToken);
        engine.on_streaming_complete(DriverTwinProbeToken);
        engine.on_resolve(DriverTwinProbeToken);
        engine.on_solve(DriverTwinProbeToken);
        engine.on_simulate(DriverTwinProbeToken);
        engine.on_gate(DriverTwinProbeToken);
        engine.on_publish(DriverTwinProbeToken);
        engine.on_finalize(DriverTwinProbeToken);
        engine.on_rewind(DriverTwinProbeToken);
        engine.has_dirty_paths(DriverTwinProbeToken);
        engine.notify_block(DriverTwinProbeToken);
    }

    /// Minimal `tracing_subscriber::Layer` that records ERROR events
    /// (target + message). Same pattern as the `ReorgSpanCapture` layer in
    /// `block_pump.rs` tests: a real subscriber through
    /// `tracing::subscriber::with_default`, not a mocked logger, so the pin
    /// observes the actual `op_error!` dispatch.
    #[derive(Clone, Default)]
    struct LoudCloseCapture {
        events: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl LoudCloseCapture {
        fn saw_loud_close(&self) -> bool {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|(target, message)| {
                    target.contains("solver") && message.contains("pump ended")
                })
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LoudCloseCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            if *event.metadata().level() != tracing::Level::ERROR {
                return;
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((event.metadata().target().to_string(), message.0));
        }
    }

    /// Pin 3 loudness half (GREEN at HEAD). The `EngineStages` pump-ended
    /// close emits the `op_error!` loud-close log. Today the log is on the
    /// `StageHandlers::on_pump_ended` hook; T2 moves it (with the poke) to
    /// `PumpControl::on_pump_ended` and deletes the non-logging inherent
    /// twin, so this captures the load-bearing behavior that must SURVIVE the
    /// T2/T3 deletions — a silent close would turn this red.
    #[test]
    fn candidate2_enginestages_pump_ended_logs_loudly() {
        use tracing_subscriber::layer::SubscriberExt;
        let stages = super::EngineStages::new(
            Arc::new(parking_lot::Mutex::new(super::ArbitrageEngine::new())),
            Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        let capture = LoudCloseCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            crate::bot_core::PumpControl::on_pump_ended(&stages);
        });
        assert!(
            capture.saw_loud_close(),
            "EngineStages::on_pump_ended must emit the op_error loud-close log (it must survive T2/T3)"
        );
    }
}

// ======================================================================
// ergo 3FA7CN — RED pin for the construction-injected epoch ledger.
// Written against the TARGET contract; production code is NOT changed.
// ======================================================================
#[cfg(test)]
mod construction_ledger_pins {
    use std::sync::Arc;

    /// Pin A (behavioral): `EngineStages` takes the ONE Bot-owned ledger at
    /// construction. The injected Arc IS the handle the stage surface reads
    /// (`PumpControl::has_dirty_paths`) — recording into it is visible
    /// through the stage surface with no `set_delta` swap.
    #[test]
    fn construction_injects_the_one_ledger() {
        let delta = Arc::new(crate::bot_core::EpochDelta::new(0u64));
        let stages = super::EngineStages::new(
            Arc::new(parking_lot::Mutex::new(super::ArbitrageEngine::new())),
            Arc::clone(&delta),
        );
        assert!(
            !crate::bot_core::PumpControl::has_dirty_paths(&stages),
            "a fresh injected ledger is empty"
        );
        delta.record_affected(degenbot_solvers::mixed::HopType::V2, 1u64, 1u64);
        assert!(
            crate::bot_core::PumpControl::has_dirty_paths(&stages),
            "the injected ledger must be the one the stage surface reads"
        );
    }

    /// Pin B (compile-time absence, candidate2's `NoInherentTwinProbe`
    /// pattern): `set_delta` / `delta_for_test` are the swap surface this
    /// task retires. An inherent re-introduction would shadow the probe and
    /// fail to compile (arity/type mismatch).
    #[test]
    fn construction_has_no_swap_surface() {
        struct SwapProbeToken;
        trait NoSwapSurfaceProbe {
            fn set_delta(&self, _t: SwapProbeToken);
            fn delta_for_test(&self, _t: SwapProbeToken);
        }
        impl NoSwapSurfaceProbe for super::EngineStages {
            fn set_delta(&self, _t: SwapProbeToken) {}
            fn delta_for_test(&self, _t: SwapProbeToken) {}
        }
        let stages = super::EngineStages::new(
            Arc::new(parking_lot::Mutex::new(super::ArbitrageEngine::new())),
            Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        stages.set_delta(SwapProbeToken);
        stages.delta_for_test(SwapProbeToken);
    }
}

// ======================================================================
// ergo 5WCRWZ T7 — THE final structural gate for the solver_dispatch
// dissolution (epic 5WCRWZ, slices T1–T7).
//
// Provenance: T1 moved the heavy-path capture diagnostics to
// `arb_engine::solver_capture`; T2 the workload partition to
// `arb_engine::workload_partition`; T3/T4 the lane walk to
// `arb_engine::lane_walk`; T5 the statics/ride consumers to their
// consuming modules; T6 the detached-merge sidecar to
// `arb_engine::detached_cycle` and the executor A/B fixtures to
// `arb_engine::executor_ab_probe`; T7 collapses the engine twins and
// DELETES `arb_engine/solver_dispatch.rs` outright (hard cutover).
//
// This replaces the four per-slice `include_str!("solver_dispatch.rs")`
// honesty probes: a single `include_str!("mod.rs")` absence check (the
// module tree owns no such module) plus spot checks that the surviving
// homes define the items T7 reallocated. Textual include_str! keeps the pin
// compile-error-free (the prior hard-cutover probe form).
// ======================================================================
#[cfg(test)]
mod dissolution_complete {
    const MOD_RS: &str = include_str!("mod.rs");
    const EVENT_ROUTING: &str = include_str!("event_routing.rs");
    const LANE_WALK: &str = include_str!("lane_walk.rs");
    const SOLVE_CYCLE: &str = include_str!("solve_cycle.rs");

    /// The module tree no longer declares (or even names) `solver_dispatch`.
    #[test]
    fn solver_dispatch_is_gone_from_the_module_tree() {
        assert!(
            !MOD_RS.contains("mod solver_dispatch;"),
            "the arb_engine module tree still declares solver_dispatch (5WCRWZ T7)"
        );
        assert!(
            !MOD_RS.contains("solver_dispatch"),
            "arb_engine/mod.rs still names solver_dispatch — T7 owns the final cleanup"
        );
    }

    /// Every reallocated item is owned by its surviving home.
    #[test]
    fn survivors_own_their_reallocated_items() {
        // The walk-adjacent helpers moved to lane_walk.
        for marker in [
            "pub(crate) fn clamp_result_in_worker(",
            "pub(crate) fn flush_solved_item(",
        ] {
            assert!(
                LANE_WALK.contains(marker),
                "lane_walk.rs must own {marker:?} (5WCRWZ T7)"
            );
        }
        // The clamp body + its profit recompute moved to solve_cycle.
        for marker in [
            "fn clamp_result_with_state(",
            "fn recompute_clamped_profit(",
        ] {
            assert!(
                SOLVE_CYCLE.contains(marker),
                "solve_cycle.rs must own {marker:?} (5WCRWZ T7)"
            );
        }
        // The detached-merge engine surface moved to event_routing, which
        // now chains directly to the cycle (no engine twin).
        for marker in [
            "pub(crate) fn merge_detached_item(",
            "self.cycle.run_epoch(",
        ] {
            assert!(
                EVENT_ROUTING.contains(marker),
                "event_routing.rs must own {marker:?} (5WCRWZ T7)"
            );
        }
        // The Default impl moved beside the engine struct.
        assert!(
            MOD_RS.contains("impl Default for ArbitrageEngine"),
            "arb_engine/mod.rs must own the ArbitrageEngine Default impl (5WCRWZ T7)"
        );
    }

    /// Compile-time inherent-absence probe (the candidate2 `NoInherentTwinProbe`
    /// pattern): the collapsed engine twins must not reappear as inherent
    /// `ArbitrageEngine` methods. An inherent re-introduction would shadow
    /// the probe and fail to compile (arity/type mismatch).
    #[test]
    fn arbitrage_engine_has_no_inherent_twin_surface() {
        struct ModelTwinProbeToken;
        trait NoInherentTwinProbe {
            fn rebuild_and_solve_affected(&self, _t: ModelTwinProbeToken);
            fn solve_all(&self, _t: ModelTwinProbeToken);
            fn admission_budget_keys(&self, _t: ModelTwinProbeToken);
            fn clamp_cl_hop_capacity(&self, _t: ModelTwinProbeToken);
            fn clamp_result_with_state(&self, _t: ModelTwinProbeToken);
        }
        impl NoInherentTwinProbe for super::ArbitrageEngine {
            fn rebuild_and_solve_affected(&self, _t: ModelTwinProbeToken) {}
            fn solve_all(&self, _t: ModelTwinProbeToken) {}
            fn admission_budget_keys(&self, _t: ModelTwinProbeToken) {}
            fn clamp_cl_hop_capacity(&self, _t: ModelTwinProbeToken) {}
            fn clamp_result_with_state(&self, _t: ModelTwinProbeToken) {}
        }
        let engine = super::ArbitrageEngine::new();
        engine.rebuild_and_solve_affected(ModelTwinProbeToken);
        engine.solve_all(ModelTwinProbeToken);
        engine.admission_budget_keys(ModelTwinProbeToken);
        engine.clamp_cl_hop_capacity(ModelTwinProbeToken);
        engine.clamp_result_with_state(ModelTwinProbeToken);
    }
}
