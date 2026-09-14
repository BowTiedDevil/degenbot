//! THE one detached solve-arm machine (P37YJG, epic SRQEK5 lineage).
//!
//! The detached solve arm of [`crate::arb_engine::ArbitrageEngine`] is ONE
//! conceptual per-cycle machine. This module is its single owner: the
//! per-cycle states (`Unopened → Open`), the merge pipe open/take, the
//! outstanding-gauge pair, the seq counters, the outcome-ledger door, the
//! disposition counters, and the ONE sidecar spawn. WFF6MM hard cutover:
//! the in-cycle fallback arm (and its stance input) is GONE — every solve
//! cycle issues the detached arm.
//!
//! # States
//!
//! - [`CycleArm::Unopened`] — no detached cycle ever issued; the pipe is
//!   closed (the never-yet-detached engine).
//! - [`CycleArm::Open`] — a detached cycle has issued; the merge pipe is
//!   open and exactly one `Receiver` parks until the sidecar takes it.
//!
//! # Transition discipline
//!
//! One legal-transition table ([`transition`]) + the sized
//! [`ALL_CYCLE_ARMS`] const + the conformance walk in the test module
//! (house pattern: `degenbot-workers` `slot.rs` T1–T9 +
//! `bot_core::stage_handlers::ALL_STAGES`). WFF6MM: every row is total (a
//! begin always opens; gauge/disposition events are state-transparent), so
//! the table can no longer reject — the typed-rejection machinery retired
//! with the in-cycle arm. The panics and aborts that exist today stay
//! verbatim (ADR-042 §10 deadlock-ledger / loud-stop discipline: stranded
//! merge pipe, vanished pipe — same log wording, same `std::process::abort`).
//!
//! # Lock order (preserved verbatim)
//!
//! The engine mutex stays the OUTER lock; the ledger mutex
//! ([`DetachedCycle::outcome_ledger`]) is always an inner lock — never the
//! reverse (no ABBA ordering). See the verbatim note on
//! `executor::outcome_ledger::OutcomeLedger::claim`.
//!
//! _Avoid_: "detached arm plumbing", "sidecar state" (CONTEXT.md).

use degenbot_core::op_error;
// ---------------------------------------------------------------------------
// The machine surface — states, verbs, the total transition table
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::executor::outcome_ledger::OutcomeLedger;
use super::executor::LaneOutcome;
use super::ArbitrageEngine;

/// Design-locked in-flight depth safety valve (~8). WFF6MM: this is NO
/// longer a runtime cap verdict (the in-cycle degrade it gated is retired);
/// it survives only as the default + construction clamp for
/// `solve.admission_target_depth`. The admission draw
/// (`budget = max(0, admission_target_depth − in-flight)`) is now the sole
/// backpressure, and `admission_target_depth` is itself clamped to this
/// value so an operator can never raise the pipe depth past the design cap.
/// (P37YJG: the cap consult moved into the machine; WFF6MM: the consult
/// retired with the arm.)
pub(crate) const DETACHED_INFLIGHT_CAP: u64 = 8;

/// THE persistent machine state (P37YJG). WFF6MM: the in-flight-cap
/// `Saturated` state retired with the in-cycle arm — a begin ALWAYS opens,
/// so the machine is `Unopened → Open`; no row ever returns to
/// [`CycleArm::Unopened`] (a pipe, once open, stays open until teardown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CycleArm {
    /// No detached cycle ever issued; the merge pipe is closed.
    Unopened,
    /// A detached cycle has issued; the pipe is open (one parked `Receiver`).
    Open,
}

/// Every machine state, in cycle order — the sized ALL-states const the
/// conformance walk drives (house pattern: `stage_handlers::ALL_STAGES` /
/// `slot.rs` `ALL_ROLES`). Adding a [`CycleArm`] variant without extending
/// the table + the walk fails the test module's exhaustive match.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the ALL-states const is the conformance walk's driver — the walk is test-declared only (house discipline)"
    )
)]
pub(crate) const ALL_CYCLE_ARMS: [CycleArm; 2] = [CycleArm::Unopened, CycleArm::Open];

/// One terminal disposition of ONE detached outcome (the machine's
/// disposition counters + the pipeline meters they feed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// The straggler merged (apply-if-unchanged): `applied`.
    Applied,
    /// Q1a stale drop (a pool ticked during the solve): `dropped_stale`.
    DroppedStale,
    /// The path deregistered (or a typed `Failed` record landed — a genuine
    /// final drop): `dropped_deregistered`.
    DroppedDeregistered,
    /// The exactness fuse tripped (a duplicate `(solve_seq, pid)` delivery
    /// was refused): `duplicate_outcomes`.
    Duplicate,
}

/// A machine verb — the drivers' complete surface, one row family each in
/// [`transition`]. WFF6MM: the in-cycle `TickInCycle` verb retired with the
/// arm; a begin carries no verdict (the admission draw owns backpressure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transition {
    /// [`DetachedCycle::begin_cycle`] — the arm decision. WFF6MM: the
    /// detached arm is the ONLY arm; a begin always opens the pipe (no
    /// stance input, no cap verdict — the admission draw owns backpressure).
    BeginCycle,
    /// [`DetachedCycle::gauge_hook`] fired — one Solved outcome's
    /// send-success bump (the ISSUE half of the gauge pair).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a state-transparent G-row: the bump is a cross-thread atomic event, constructed only by the conformance walk"
        )
    )]
    OutcomeSent,
    /// [`DetachedCycle::disposition`] — one terminal disposition.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a state-transparent D-row: the counters are process-cumulative, constructed only by the conformance walk"
        )
    )]
    Disposition(Disposition),
}

/// THE legal-transition table (P37YJG; house pattern:
/// `degenbot-workers` `slot.rs::transition`). WFF6MM: with the in-cycle arm
/// retired every row is TOTAL, so the table is infallible (no typed
/// rejections remain):
///
/// ```text
///              BeginCycle   OutcomeSent / Disposition(k)
/// Unopened  →  Open         unchanged
/// Open      →  Open         unchanged
/// ```
///
/// The G/D-rows are state-transparent ON PURPOSE: the gauge pair
/// (send-success bump ⟺ merge receipt decrement) and the process-cumulative
/// disposition counters are cross-thread events the persistent state does
/// not gate (the sidecar lands items in whatever state the engine is in;
/// the direct-merge test harness drives dispositions on dormant machines).
#[must_use]
pub(crate) fn transition(from: CycleArm, t: Transition) -> CycleArm {
    match t {
        // B-row — the arm decision. A begin always issues the detached arm.
        Transition::BeginCycle => CycleArm::Open,
        // G/D-rows — cross-thread gauge + disposition events.
        Transition::OutcomeSent | Transition::Disposition(_) => from,
    }
}

/// The per-cycle begin decision: the machine-issued seq (THE ledger key half
/// for this cycle's detached claims — the ONE `(solve_seq, pid)` key zone)
/// and the merge-pipe `Sender` clone for the 'static bin threads. WFF6MM:
/// the detached arm is the ONLY arm — the in-cycle alternative retired with
/// its stance and seq-tick verb.
#[derive(Debug)]
pub(crate) struct DetachedArm {
    /// The seq this detached cycle was stamped with (`solve_seq_ctr` after
    /// the tick — the ONE counter).
    pub(crate) cycle_seq: u64,
    /// A clone of the merge pipe's `Sender` (opened once, on the first
    /// detached cycle).
    pub(crate) merge_tx: std::sync::mpsc::Sender<LaneOutcome>,
}

/// The drain's counter aggregate (the sidecar's per-item consumption).
/// P37YJG: the machine owns the disposition bookkeeping, so the aggregate
/// lives here.
#[derive(Default)]
pub(crate) struct LaneDrainCounts {
    pub(crate) solved: usize,
    pub(crate) suppressed: usize,
    pub(crate) failed: usize,
}

// ---------------------------------------------------------------------------
// THE machine
// ---------------------------------------------------------------------------

/// THE one detached solve-arm machine (P37YJG): the single owner of the
/// scattered per-cycle fields this module's doc header names. The engine
/// holds ONE of these. WFF6MM: the in-cycle stance is gone — every begin
/// issues the detached arm.
/// Lock order: the engine mutex (which guards this whole struct) is the
/// OUTER lock; [`Self::outcome_ledger`]'s mutex is always an inner lock —
/// never the reverse (no ABBA ordering). The gauge atomics are lock-free.
pub(crate) struct DetachedCycle {
    /// The persistent state (see [`CycleArm`]).
    state: CycleArm,
    /// Monotonic counter bumped per issued detached SOLVE cycle (it is THE
    /// ledger's seq half). The sidecar's straggler-age telemetry reads
    /// `detached_issued_seq` against it.
    solve_seq_ctr: u64,
    /// The seq of the most recently issued detached cycle.
    detached_issued_seq: u64,
    /// Sender half of the UNBOUNDED mpsc merge pipe; `Some` from the first
    /// detached enqueue until teardown. Each enqueue clones it into the
    /// per-bin bin jobs. Carries the unified [`executor::LaneOutcome`]
    /// (QR3NUS 43E3H3).
    merge_tx: Option<std::sync::mpsc::Sender<LaneOutcome>>,
    /// Receiver parked until `EngineStages::solve_dirty` spawns the merge
    /// sidecar (taken once via [`Self::take_merge_rx`]). `Mutex`-wrapped so
    /// the engine stays `Sync` (the parked Receiver behind the worker-only
    /// guard is touched exactly once, by the spawner thread).
    merge_rx: parking_lot::Mutex<Option<std::sync::mpsc::Receiver<LaneOutcome>>>,
    /// LW-T9 note-(a) carry: the duplicate-outcome fuse counter (the QR3NUS
    /// exactness assert). 43E3H3: the sidecar's ledger claims feed it — one
    /// process-cumulative count.
    pub(crate) duplicate_outcomes: std::sync::atomic::AtomicU64,
    /// THE exactness ledger (LW-T9 note (a) -> QR3NUS 43E3H3): one outcome
    /// per (`solve_seq`, path) EXACTLY once — keyed (`solve_seq`, pid); the
    /// sidecar claims under the enqueue-stamped `cycle_seq`. Held on the
    /// ENGINE (`parking_lot` Mutex) — the fuse is stateful across sidecar
    /// restarts (the pipe outlives any one sidecar thread).
    /// P37YJG: the machine OWNS and drives the field (via [`Self::claim`]);
    /// the TYPE stays `executor::outcome_ledger::OutcomeLedger`.
    pub(crate) outcome_ledger: parking_lot::Mutex<OutcomeLedger>,
    /// In-flight gauge: detached results SENT but not yet dispositioned.
    /// `Arc` because the enqueue half's bin threads bump it at send time
    /// ([`Self::gauge_hook`]) and the sidecar decrements it per Solved
    /// receipt ([`Self::solved_received`]). WFF6MM: the gauge feeds the
    /// admission draw (`budget = max(0, admission_target_depth − in-flight)`),
    /// which is the sole backpressure now.
    pub(crate) outstanding: Arc<std::sync::atomic::AtomicU64>,
    /// Detached straggler outcome counters (applied / stale-dropped /
    /// deregistered-dropped); T2 wires the `detached.*` metrics from these.
    pub(crate) applied: std::sync::atomic::AtomicU64,
    pub(crate) dropped_stale: std::sync::atomic::AtomicU64,
    pub(crate) dropped_deregistered: std::sync::atomic::AtomicU64,
    /// QTZGFL: cycles SHED by capacity-modulated admission (zero draw
    /// budget) — the machine's disposition counter behind
    /// `degenbot.detached.shed`. A shed cycle submits nothing and
    /// claims nothing (no seq tick); this is a pure counter event.
    pub(crate) shed_cycles: std::sync::atomic::AtomicU64,
    /// QTZGFL: retained (carried) admission keys pruned by the retention
    /// window (`head − W`) — the machine's counter behind
    /// `degenbot.detached.leads_expired`.
    pub(crate) leads_expired: std::sync::atomic::AtomicU64,
}

impl Default for DetachedCycle {
    fn default() -> Self {
        Self::new()
    }
}

impl DetachedCycle {
    /// The pre-cycle init: dormant (`Unopened`), pipe closed, counters at 0.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: CycleArm::Unopened,
            solve_seq_ctr: 0,
            detached_issued_seq: 0,
            merge_tx: None,
            merge_rx: parking_lot::Mutex::new(None),
            duplicate_outcomes: std::sync::atomic::AtomicU64::new(0),
            outcome_ledger: parking_lot::Mutex::new(OutcomeLedger::default()),
            outstanding: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            applied: std::sync::atomic::AtomicU64::new(0),
            dropped_stale: std::sync::atomic::AtomicU64::new(0),
            dropped_deregistered: std::sync::atomic::AtomicU64::new(0),
            shed_cycles: std::sync::atomic::AtomicU64::new(0),
            leads_expired: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The persistent machine state (diagnostics + the conformance walk).
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the state probe is the conformance walk's + diagnostics' view; production reads the arm decision, not the state"
        )
    )]
    pub(crate) fn state(&self) -> CycleArm {
        self.state
    }

    /// The most recently issued detached cycle's seq — the sidecar's
    /// straggler-age telemetry anchor (`detached_issued_seq`).
    #[must_use]
    pub(crate) fn issued_seq(&self) -> u64 {
        self.detached_issued_seq
    }

    /// THE arm decision (one machine verb): tick the ONE seq counter, open
    /// the merge pipe once (if not already open), and hand back the `Sender`
    /// clone for the 'static bin threads. WFF6MM: this ALWAYS issues the
    /// detached arm — the stance input, the live-gauge cap verdict, and the
    /// in-cycle fallback retired; the admission draw owns backpressure.
    ///
    /// The row is total — the table cannot reject this.
    pub(crate) fn begin_cycle(&mut self) -> DetachedArm {
        self.state = transition(self.state, Transition::BeginCycle);
        // THE DETACHED ISSUE: tick the ONE counter and stamp the
        // detached-only telemetry anchor.
        self.solve_seq_ctr += 1;
        self.detached_issued_seq = self.solve_seq_ctr;
        // The merge pipe: open ONCE (the first detached cycle). The sidecar
        // thread is spawned by EngineStages::solve_dirty right after this
        // enqueue half returns; the Receiver parks in the machine until
        // then.
        if self.merge_tx.is_none() {
            let (merge_tx, merge_rx) = std::sync::mpsc::channel();
            self.merge_tx = Some(merge_tx);
            *self.merge_rx.lock() = Some(merge_rx);
        }
        // Clone the Sender out so the 'static bin threads never borrow the
        // engine (they outlive the call). A vanished pipe would strand
        // every result, so die loudly (ADR-042 §10 — verbatim).
        let merge_tx = if let Some(existing) = &self.merge_tx {
            existing.clone()
        } else {
            // unreachable-by-construction (opened above); a vanished
            // pipe would strand every result, so die loudly.
            op_error!(
                domain = solver,
                "merge pipe vanished between open and clone — aborting"
            );
            std::process::abort();
        };
        DetachedArm {
            cycle_seq: self.detached_issued_seq,
            merge_tx,
        }
    }

    /// The ISSUE half of the gauge pair (contract 1, REV 2 Defect 1): ONE
    /// `Arc` hook per bin, fired on a `Solved` item's SEND SUCCESS only —
    /// never for `Suppressed`/`Failed` (those never bump, so they may never
    /// decrement). The hook is 'static (bin threads outlive the cycle; an
    /// `Arc`-shared atomic carries the bump).
    #[must_use]
    pub(crate) fn gauge_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        let outstanding = Arc::clone(&self.outstanding);
        Arc::new(move || {
            outstanding.fetch_add(1, Ordering::Relaxed);
        })
    }

    /// The RECEIPT half of the gauge pair: ONE Solved item arrived at the
    /// merge — decrement the in-flight gauge exactly once and publish both
    /// meters. ONLY the Solved arm calls this (only it was ever bumped).
    /// Returns the post-decrement count for the caller's logs.
    pub(crate) fn solved_received(&self) -> u64 {
        let outstanding_now = self
            .outstanding
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        if let Some(p) = crate::instruments::pipeline() {
            p.set_detached_in_flight(outstanding_now);
        }
        hotpath::gauge!("detached_solve_in_flight").set(f64::from(
            u32::try_from(outstanding_now).unwrap_or(u32::MAX),
        ));
        outstanding_now
    }

    /// Publish the in-flight gauge to both meters (the enqueue half's
    /// post-submit read).
    pub(crate) fn publish_gauge(&self) {
        let outstanding_now = self.outstanding.load(Ordering::Relaxed);
        if let Some(p) = crate::instruments::pipeline() {
            p.set_detached_in_flight(outstanding_now);
        }
        hotpath::gauge!("detached_solve_in_flight").set(f64::from(
            u32::try_from(outstanding_now).unwrap_or(u32::MAX),
        ));
    }

    /// QTZGFL: one admission SHED cycle (zero draw budget) — the machine
    /// owns the disposition counter + its pipeline meter, mirroring
    /// [`Self::disposition`]. A shed cycle takes NO transition beyond the
    /// begin (the machine is not consulted at all on the deterministic path);
    /// this is a pure counter event.
    pub(crate) fn shed(&self) {
        self.shed_cycles.fetch_add(1, Ordering::Relaxed);
        if let Some(p) = crate::instruments::pipeline() {
            p.count_detached_shed();
        }
    }

    /// QTZGFL: `n` retained admission keys expired by the retention window
    /// (`head − W`) on a block advance — the machine counter + its pipeline
    /// meter. A zero count is a no-op (no spurious series touch).
    pub(crate) fn note_leads_expired(&self, n: usize) {
        let expired = u64::try_from(n).unwrap_or(u64::MAX);
        if expired == 0 {
            return;
        }
        self.leads_expired.fetch_add(expired, Ordering::Relaxed);
        if let Some(p) = crate::instruments::pipeline() {
            p.count_detached_leads_expired(expired);
        }
    }

    /// ONE terminal disposition: land it on the machine's counter (+ the
    /// pipeline meter it feeds). The per-item LOG LINES stay at the call
    /// sites (they carry item fields — path id, seq, age — and their span
    /// parents).
    pub(crate) fn disposition(&self, kind: Disposition) {
        match kind {
            Disposition::Applied => {
                self.applied.fetch_add(1, Ordering::Relaxed);
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_detached_applied();
                }
            }
            Disposition::DroppedStale => {
                self.dropped_stale.fetch_add(1, Ordering::Relaxed);
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_detached_stale_dropped();
                }
            }
            Disposition::DroppedDeregistered => {
                self.dropped_deregistered.fetch_add(1, Ordering::Relaxed);
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_detached_stale_dropped();
                }
            }
            Disposition::Duplicate => {
                self.duplicate_outcomes.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// THE one ledger door (P37YJG): the machine drives the ledger — every
    /// arm's claim runs through here with the machine-issued seq half.
    /// Callers hold the engine mutex across `claim` (the ledger mutex is
    /// always an inner lock — never the reverse: no ABBA ordering; see the
    /// verbatim note on `OutcomeLedger::claim`).
    pub(crate) fn claim(&self, k: (u64, u64)) -> Result<(), (u64, u64)> {
        self.outcome_ledger.lock().claim(k)
    }

    /// Hand the parked merge-pipe Receiver to the spawner (epic SRQEK5
    /// WV62TX): `EngineStages::solve_dirty` takes it ONCE, at the FIRST
    /// detached enqueue, and owns it inside the sidecar thread. `None` = the
    /// sidecar is already running (or no detached cycle ever enqueued).
    pub(crate) fn take_merge_rx(&mut self) -> Option<std::sync::mpsc::Receiver<LaneOutcome>> {
        self.merge_rx.lock().take()
    }
}

// ---------------------------------------------------------------------------
// THE ONE sidecar spawn (P37YJG): both former engine_stages spawn sites
// delegate here.
// ---------------------------------------------------------------------------

/// The detached merge sidecar's thread name (the sidecar IS the fleet
/// `Merge` role — the pinned T4 seat's named thread pattern; the historical
/// legacy name retired at the LW-T9 cutover).
#[must_use]
pub(crate) fn merge_sidecar_thread_name() -> String {
    degenbot_workers::role::WorkerRole::Merge
        .thread_name()
        .replace("{n}", "1")
}

/// The sidecar's worker-census row: the fleet `Merge` role's row (census
/// resource `fleet_merge_slots`, exactly one pinned seat) — the only
/// posture since the LW-T9 cutover.
#[must_use]
pub(crate) fn merge_sidecar_census_entry() -> degenbot_core::worker_census::WorkerCensusEntry {
    let role = degenbot_workers::role::WorkerRole::Merge;
    degenbot_core::worker_census::WorkerCensusEntry {
        resource: role.census_resource(),
        kind: role.census_kind(),
        count: 1,
        thread_name: role.thread_name(),
        sizing: role.census_sizing(),
        binding: "pinned",
    }
}

// ---------------------------------------------------------------------------
// DETACHED SOLVE CYCLE (epic SRQEK5, task WV62TX)
// ---------------------------------------------------------------------------
// DETACH-ALWAYS (design locked 2026-09-02; WFF6MM cutover retired the
// in-cycle arm so this is now the unconditional shape): the whole solve
// cycle RETURNS at ENQUEUE end — every result then flows through an
// UNBOUNDED mpsc to the merge sidecar, a plain `std::thread` (see the
// epic DEADLOCK note: a JOINING scope (a scoped rayon install of old, or
// against a held `parking_lot` guard; `std::thread` cannot deadlock with
// impatient pool) would starve against the Mutex; the sidecar cannot. The Q1a stale policy
// (apply-if-unchanged / drop-on-touched) makes the enqueue-time per-hop
// `update_block` snapshot a complete staleness oracle: a price-neutral
// liquidity event (V3 Mint/Burn, V4 ModifyLiquidity) advances the pool
// clock AND re-solves the path, so any stamp mismatch at merge time means
// the straggler's intake is stale and the result is DROPPED, never applied.
// This gate is now the SOLE staleness guard on the solve path (the ADR-021
// in-process solver-state tripwire retired with task 2UVG3E; only the
// upstream RPC-disagreement check survives at the Published edge).

// P37YJG: the in-flight cap constant moved with the cap consult into the
// one detached-cycle machine — `detached_cycle::DETACHED_INFLIGHT_CAP`.

// The detached-merge CARRIER is `executor::LaneOutcome` (QR3NUS 43E3H3):
// the former single-variant enum folded into the unified
// `LaneOutcome::Solved(SolveOutcome)` — the typed `Solved`/`Suppressed`/
// `Failed` records both solve arms deliver (the sidecar's
// pipe now also carries the lane witness's `Failed` panic records). The
// exactness ledger age moved with it (`executor::outcome_ledger::LEDGER_AGE`,
// carried unchanged).

/// The detached-merge SIDECAR thread body (epic SRQEK5 WV62TX): owns the
/// unbounded mpsc `Receiver` of the merge pipe and applies each item under
/// the engine Mutex — Q1a stale gate + the SAME merge/emit path as the
/// in-cycle drain (`merge_one_result`, which carries the streaming
/// delivery emission). Spawned by `EngineStages::solve_dirty` at the FIRST
/// detached enqueue; runs until every `Sender` drops (engine teardown),
/// so the pipe never strands items across the engine's lifetime.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the sidecar OWNS the merge Receiver for the engine's whole lifetime (the pipe must never be dropped early or borrowed from a shared slot); owning it is the contract, not an accident"
)]
pub(crate) fn detached_merge_sidecar(
    engine: &std::sync::Arc<parking_lot::Mutex<ArbitrageEngine>>,
    merge_rx: std::sync::mpsc::Receiver<LaneOutcome>,
    owner: Option<&degenbot_workers::posture::PostureOwner>,
) {
    hotpath::measure_block!("arb_solve.detached_merge", {
        // `recv` (not `for .. in merge_rx`) keeps ownership of the
        // Receiver so the post-panic stranded-tail drain can `try_iter`.
        while let Ok(item) = merge_rx.recv() {
            // AQV6EF: a panicking merge must NEVER silently kill this
            // thread — that drops the Receiver and strands every later
            // send with no signal. catch_unwind converts the panic into
            // the SAME typed drain-death terminal state as a failed send
            // (sticky cordon + counter + loud log); the process lives.
            let merged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                engine.lock().merge_detached_item(item);
            }));
            if let Err(payload) = merged {
                let message = if let Some(text) = payload.downcast_ref::<&str>() {
                    Some((*text).to_owned())
                } else {
                    payload.downcast_ref::<String>().cloned()
                };
                crate::arb_engine::executor::drain_death_response(
                    &crate::arb_engine::executor::DrainFailure::MergePanic { message },
                    owner,
                );
                // The drain can never recover in-process. Count every
                // outcome still queued behind the panicked item before the
                // Receiver drops (the unbounded-queue tail), then end the
                // sidecar; later sends hit the dead pipe and fire the SAME
                // typed signal through the lane hook.
                for stranded in merge_rx.try_iter() {
                    crate::arb_engine::executor::drain_death_response(
                        &crate::arb_engine::executor::DrainFailure::Stranded {
                            pid: stranded.pid(),
                        },
                        owner,
                    );
                }
                return;
            }
        }
    });
}

/// Spawn the detached merge sidecar for the parked receiver (epic SRQEK5
/// WV62TX; P37YJG: THE ONE spawn — both former `engine_stages` sites call
/// this). The take-once is [`DetachedCycle::take_merge_rx`], done by the
/// caller under whatever engine hold it already owns; this fn registers the
/// pinned seat and starts the named thread. A spawn failure LOUDLY ABORTS:
/// a stranded merge pipe would silently orphan every detached result
/// (ADR-042 §10 — wording + abort verbatim).
pub(crate) fn spawn_merge_sidecar(
    engine: &std::sync::Arc<parking_lot::Mutex<super::ArbitrageEngine>>,
    merge_rx: std::sync::mpsc::Receiver<LaneOutcome>,
) {
    let engine_arc = std::sync::Arc::clone(engine);
    // PE4FPM: self-register the pinned merge sidecar (the fleet
    // Merge role — the only posture since the LW-T9 cutover).
    degenbot_core::worker_census::register(merge_sidecar_census_entry());
    if let Err(err) = std::thread::Builder::new()
        .name(merge_sidecar_thread_name())
        .spawn(move || {
            // AQV6EF: production uses the process posture owner (None).
            detached_merge_sidecar(&engine_arc, merge_rx, None);
        })
    {
        // LOUD abort: a stranded merge pipe would silently orphan
        // every detached result.
        op_error!(domain = solver, error = %err,
            "detached merge sidecar spawn failed — aborting (stranded merge pipe)"
        );
        std::process::abort();
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::arb_engine::executor;

    /// The sized ALL-states const is exhaustive and duplicate-free; adding
    /// a `CycleArm` variant without extending the table + this walk fails
    /// the exhaustive match below at compile time (house pattern:
    /// `stage_handlers::ALL_STAGES` / `slot.rs` `ALL_ROLES`).
    #[test]
    fn all_cycle_arms_covers_every_state_exactly_once() {
        assert_eq!(ALL_CYCLE_ARMS.len(), 2, "the machine declares 2 states");
        for (i, s) in ALL_CYCLE_ARMS.iter().enumerate() {
            assert!(
                !ALL_CYCLE_ARMS[i + 1..].contains(s),
                "duplicate state {s:?} in ALL_CYCLE_ARMS"
            );
        }
        for s in ALL_CYCLE_ARMS {
            // Exhaustive: a new variant breaks this match at compile time.
            match s {
                CycleArm::Unopened | CycleArm::Open => {}
            }
        }
        assert_eq!(ALL_CYCLE_ARMS, [CycleArm::Unopened, CycleArm::Open]);
    }

    /// THE CONFORMANCE WALK (P37YJG): every legal (state × transition) cell
    /// lands on its table successor. WFF6MM: with the in-cycle arm retired
    /// every row is total — there is no illegal cell and therefore no typed
    /// rejection left to pin.
    #[test]
    fn conformance_walks_every_legal_transition() {
        for from in ALL_CYCLE_ARMS {
            // BeginCycle — the arm decision. Total: every solve cycle
            // begins, from any state, and opens the pipe.
            assert_eq!(
                transition(from, Transition::BeginCycle),
                CycleArm::Open,
                "B-row from {from:?}: the detached issue opens"
            );
            // Cross-thread gauge + disposition events: state-transparent
            // (the pair — bump ⟺ receipt — and the process-cumulative
            // counters are NOT gated by the persistent state; the sidecar
            // lands items in whatever state the engine is in).
            assert_eq!(
                transition(from, Transition::OutcomeSent),
                from,
                "G-row from {from:?}: the send-success bump is state-transparent"
            );
            for kind in [
                Disposition::Applied,
                Disposition::DroppedStale,
                Disposition::DroppedDeregistered,
                Disposition::Duplicate,
            ] {
                assert_eq!(
                    transition(from, Transition::Disposition(kind)),
                    from,
                    "D-row {kind:?} from {from:?}: dispositions are state-transparent"
                );
            }
        }
    }

    // ---- machine-level conformance (a real machine drives the script) ----

    #[test]
    fn machine_opens_the_pipe_once_and_takes_it_once() {
        let mut m = DetachedCycle::new();
        assert_eq!(m.state(), CycleArm::Unopened);
        let DetachedArm {
            cycle_seq,
            merge_tx,
        } = m.begin_cycle();
        assert_eq!(cycle_seq, 1, "the ONE counter ticks on the detached issue");
        assert_eq!(m.state(), CycleArm::Open, "Unopened → Open");
        assert!(
            m.take_merge_rx().is_some(),
            "the first detached begin parks the receiver"
        );
        assert!(m.take_merge_rx().is_none(), "the receiver is take-ONCE");
        // Consecutive detached cycles: same pipe (no re-open), seq advances.
        let DetachedArm {
            cycle_seq: seq2, ..
        } = m.begin_cycle();
        assert_eq!(seq2, 2);
        assert_eq!(
            m.state(),
            CycleArm::Open,
            "Open → Open (consecutive cycles)"
        );
        drop(merge_tx);
    }

    /// WFF6MM: the begin ALWAYS issues the detached arm — a cycle whose
    /// in-flight gauge sits at/over the design depth safety valve still
    /// detaches (the admission draw, not a cap verdict, owns backpressure).
    #[test]
    fn machine_begins_detached_even_at_or_over_the_depth_safety_valve() {
        let mut m = DetachedCycle::new();
        for outstanding in [0, DETACHED_INFLIGHT_CAP, DETACHED_INFLIGHT_CAP + 5] {
            m.outstanding.store(outstanding, Ordering::Relaxed);
            let DetachedArm { .. } = m.begin_cycle();
            assert_eq!(m.state(), CycleArm::Open);
        }
    }

    #[test]
    fn machine_gauge_pair_and_disposition_counters() {
        let m = DetachedCycle::new();
        // The ISSUE half (the lane's send-success hook): each Solved send
        // bumps exactly once.
        let hook = m.gauge_hook();
        hook();
        hook();
        assert_eq!(m.outstanding.load(Ordering::Relaxed), 2);
        // The RECEIPT half: ONE Solved item arrived at the merge.
        assert_eq!(m.solved_received(), 1);
        assert_eq!(m.outstanding.load(Ordering::Relaxed), 1);
        // The terminal dispositions land on their counters.
        m.disposition(Disposition::Applied);
        m.disposition(Disposition::DroppedStale);
        m.disposition(Disposition::DroppedDeregistered);
        m.disposition(Disposition::Duplicate);
        m.disposition(Disposition::Duplicate);
        assert_eq!(m.applied.load(Ordering::Relaxed), 1);
        assert_eq!(m.dropped_stale.load(Ordering::Relaxed), 1);
        assert_eq!(m.dropped_deregistered.load(Ordering::Relaxed), 1);
        assert_eq!(m.duplicate_outcomes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn machine_ledger_door_claims_once_and_prunes_past_age() {
        let m = DetachedCycle::new();
        m.claim((5, 1)).expect("first sighting claims");
        assert!(
            m.claim((5, 1)).is_err(),
            "the duplicate is the fuse key — typed Err"
        );
        // The prune anchors on the CURRENT claim's seq (LEDGER_AGE = 64).
        m.claim((5 + executor::outcome_ledger::LEDGER_AGE + 1, 2))
            .expect("a far-future claim");
        assert!(
            !m.outcome_ledger.lock().contains((5, 1)),
            "rows past LEDGER_AGE prune on the next claim"
        );
    }
}
