//! THE one detached solve-arm machine.
//!
//! The detached solve arm of [`crate::arb_engine::ArbitrageEngine`] is ONE
//! conceptual per-cycle machine. This module is its single owner: the merge
//! pipe's lifecycle ([`MergePipe`]), the outstanding-gauge pair, the seq
//! counters, the outcome-ledger door, the disposition counters, and the ONE
//! sidecar spawn. Every solve cycle issues the detached arm.
//!
//! # States
//!
//! [`MergePipe`] is the merge pipe's ONE lifecycle, and its payload IS the two
//! channel halves: no caller infers the pipe's progress from an `Option`
//! combination, and no state is representable without the half it names.
//!
//! - [`MergePipe::Unopened`] — no detached cycle ever issued; no channel (the
//!   never-yet-detached engine).
//! - [`MergePipe::Parked`] — the first begin opened the channel; exactly one
//!   `Receiver` parks until the sidecar spawner takes it.
//! - [`MergePipe::Live`] — the sidecar owns that `Receiver` and drains the pipe.
//! - [`MergePipe::Dead`] — the sidecar exited; no delivery can ever land again.
//!
//! The machine's counters ([`DetachedCycle::duplicate_outcomes`] and the
//! disposition counters), the seq, and the outcome ledger stay orthogonal to
//! the pipe's state: those are per-ITEM bookkeeping, the pipe is per-PIPE, and
//! neither gates the other.
//!
//! # Transition discipline
//!
//! One legal-transition table ([`transition`]) over the payload-free
//! [`MergePipeState`] view + the sized [`ALL_MERGE_PIPE_STATES`] const + the
//! conformance walk in the test module (house pattern: `degenbot-workers`
//! `slot.rs` T1–T9 + `bot_core::stage_handlers::ALL_STAGES`). Every row is
//! total, so the table cannot reject a cycle: a begin always opens the pipe and
//! issues the detached arm (the admission draw owns backpressure), and a take
//! that finds no parked receiver hands out nothing rather than forking the
//! drain.
//!
//! A DEAD pipe refuses nothing and aborts nothing: a later begin still hands
//! out the dead channel's `Sender`, each enqueue's send then fails, and the
//! lane's send-failure hook lands every stranded outcome on the existing
//! drain-death accounting (one counted loss per outcome, the sticky
//! `PostureCause::LaneDeath` cordon, the loud throttled log). The one abort that
//! remains names a pipe that could never be drained at all — the sidecar spawn
//! failure — and it stays verbatim (the worker-fleet deadlock ledger,
//! `docs/architecture/worker-fleet.md` §10 — loud-stop discipline, same log
//! wording, same `std::process::abort`).
//!
//! # Lock order (preserved verbatim)
//!
//! The engine mutex stays the OUTER lock; the ledger mutex
//! ([`DetachedCycle::outcome_ledger`]) is always an inner lock — never the
//! reverse (no ABBA ordering). See the verbatim note on
//! `executor::outcome_ledger::OutcomeLedger::claim`.
//!
//! _Avoid_: "detached arm plumbing", "sidecar state" (GLOSSARY.md).
use degenbot_core::{op_debug, op_error};
// ---------------------------------------------------------------------------
// The machine surface — states, verbs, the total transition table
// ---------------------------------------------------------------------------
use super::executor::outcome_ledger::OutcomeLedger;
use super::executor::LaneOutcome;
use super::ArbitrageEngine;
use std::sync::atomic::Ordering;
use std::sync::Arc;
/// Design-locked in-flight depth safety valve (~8). this is NO
/// longer a runtime cap verdict (the in-cycle degrade it gated is retired);
/// it survives only as the default + construction clamp for
/// `solve.admission_target_depth`. The admission draw
/// (`budget = max(0, admission_target_depth − in-flight)`) is now the sole
/// backpressure, and `admission_target_depth` is itself clamped to this
/// value so an operator can never raise the pipe depth past the design cap.
/// (the cap consult moved into the machine; the consult
/// retired with the arm.)
pub(crate) const DETACHED_INFLIGHT_CAP: u64 = 8;
/// The merge pipe's lifecycle state — the payload-free view the diagnostics and
/// the conformance walk read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergePipeState {
    /// No detached cycle ever issued; no channel exists.
    Unopened,
    /// The channel is open and its one `Receiver` parks for the sidecar
    /// spawner.
    Parked,
    /// The sidecar owns the `Receiver` and drains the pipe.
    Live,
    /// The sidecar exited; no delivery can ever land again.
    Dead,
}
/// Every pipe state, in lifecycle order — the sized ALL-states const the
/// conformance walk drives (house pattern: `stage_handlers::ALL_STAGES` /
/// `slot.rs` `ALL_ROLES`). Adding a state without extending the table + the
/// walk fails the test module's exhaustive match.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the ALL-states const is the conformance walk's driver — the walk is test-declared only (house discipline)"
    )
)]
pub(crate) const ALL_MERGE_PIPE_STATES: [MergePipeState; 4] = [
    MergePipeState::Unopened,
    MergePipeState::Parked,
    MergePipeState::Live,
    MergePipeState::Dead,
];
/// THE merge pipe: the two channel halves ARE the state, so the pipe's progress
/// is never inferred from an `Option` combination.
///
/// Opened once (the first begin), taken once (the sidecar spawner), dead once
/// (the sidecar's terminal exit).
#[derive(Default)]
pub(crate) enum MergePipe {
    /// No detached cycle ever issued: the channel does not exist.
    #[default]
    Unopened,
    /// The first begin opened the UNBOUNDED mpsc channel; the one `Receiver`
    /// parks here until the sidecar spawner takes it. Each enqueue clones the
    /// `Sender` into its bin jobs (it carries the unified
    /// [`executor::LaneOutcome`]).
    Parked {
        tx: std::sync::mpsc::Sender<LaneOutcome>,
        rx: std::sync::mpsc::Receiver<LaneOutcome>,
    },
    /// The sidecar owns the `Receiver` and drains the pipe for the engine's
    /// lifetime.
    Live {
        tx: std::sync::mpsc::Sender<LaneOutcome>,
    },
    /// The sidecar exited: its `Receiver` is gone, so no later delivery can
    /// land. The `Sender` stays so a later cycle still hands its outcomes to
    /// this channel and each failed send surfaces the lane-death accounting.
    Dead {
        tx: std::sync::mpsc::Sender<LaneOutcome>,
    },
}
impl MergePipe {
    /// The payload-free view of the pipe's current state.
    #[must_use]
    pub(crate) fn state(&self) -> MergePipeState {
        match self {
            Self::Unopened => MergePipeState::Unopened,
            Self::Parked { .. } => MergePipeState::Parked,
            Self::Live { .. } => MergePipeState::Live,
            Self::Dead { .. } => MergePipeState::Dead,
        }
    }
    /// The begin transition: open the channel ONCE (the first begin) and hand
    /// back the `Sender` clone the 'static bin threads carry. Every later begin
    /// reuses the same channel, so ONE drain serves every cycle.
    #[must_use]
    fn begin(self) -> (Self, std::sync::mpsc::Sender<LaneOutcome>) {
        match self {
            Self::Unopened => {
                let (tx, rx) = std::sync::mpsc::channel();
                let merge_tx = tx.clone();
                (Self::Parked { tx, rx }, merge_tx)
            }
            Self::Parked { tx, rx } => {
                let merge_tx = tx.clone();
                (Self::Parked { tx, rx }, merge_tx)
            }
            Self::Live { tx } => {
                let merge_tx = tx.clone();
                (Self::Live { tx }, merge_tx)
            }
            Self::Dead { tx } => {
                let merge_tx = tx.clone();
                (Self::Dead { tx }, merge_tx)
            }
        }
    }
    /// The take transition: hand the parked `Receiver` to the sidecar spawner.
    /// Parked-only — every other state has no parked receiver, so it hands out
    /// nothing and stands unchanged (the spawn check runs after every cycle, and
    /// a second take must never fork the drain).
    #[must_use]
    fn take(self) -> (Self, Option<std::sync::mpsc::Receiver<LaneOutcome>>) {
        match self {
            Self::Parked { tx, rx } => (Self::Live { tx }, Some(rx)),
            other => (other, None),
        }
    }
    /// The sidecar's terminal exit: Live → Dead. Every other state stands
    /// unchanged — a pipe whose receiver was never taken has no sidecar to
    /// report, and a dead pipe has no second death.
    #[must_use]
    fn sidecar_exited(self) -> Self {
        match self {
            Self::Live { tx } => Self::Dead { tx },
            other => other,
        }
    }
}
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
/// A merge-pipe event — the complete set of events that move the pipe, one per
/// machine verb. The machine's verbs match every variant of this event set, and
/// the walk below is its driver (house discipline: the table const + the walk
/// are test-declared, the behavior they pin is production's).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the event set drives the conformance walk — the walk is test-declared only (house discipline)"
    )
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergePipeEvent {
    /// [`DetachedCycle::begin_cycle`]: the FIRST begin opens the channel; every
    /// later begin reuses it (same pipe, fresh `Sender` clone).
    Begin,
    /// [`DetachedCycle::take_merge_rx`]: the sidecar spawner takes the parked
    /// `Receiver` — Parked-only, so the drain can never fork.
    Take,
    /// [`DetachedCycle::note_sidecar_exited`]: the sidecar's terminal exit.
    SidecarExited,
}
/// THE legal-transition table (house pattern:
/// `degenbot-workers` `slot.rs::transition`). Every row is TOTAL, so the table
/// is infallible:
///
/// ```text
///                Begin      Take          SidecarExited
/// Unopened  →  Parked     unchanged     unchanged
/// Parked    →  unchanged  Live          unchanged
/// Live      →  unchanged  unchanged     Dead
/// Dead      →  unchanged  unchanged     unchanged
/// ```
///
/// The no-op rows are load-bearing, not gaps: a take that finds no parked
/// receiver (already spawned, never opened, or dead) hands out nothing rather
/// than forking the drain, an exit reported against a pipe no sidecar ever took
/// changes nothing, and a dead pipe refuses nothing — it only stops delivering.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the transition table is the conformance walk's driver — the walk is test-declared only (house discipline)"
    )
)]
#[must_use]
pub(crate) fn transition(from: MergePipeState, event: MergePipeEvent) -> MergePipeState {
    match (from, event) {
        (MergePipeState::Unopened, MergePipeEvent::Begin) => MergePipeState::Parked,
        (MergePipeState::Parked, MergePipeEvent::Take) => MergePipeState::Live,
        (MergePipeState::Live, MergePipeEvent::SidecarExited) => MergePipeState::Dead,
        (state, _) => state,
    }
}
/// The per-cycle begin decision: the machine-issued seq (THE ledger key half
/// for this cycle's detached claims — the ONE `(solve_seq, pid)` key zone)
/// and the merge-pipe `Sender` clone for the 'static bin threads.
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
/// the machine owns the disposition bookkeeping, so the aggregate
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
/// THE one detached solve-arm machine: the single owner of the
/// scattered per-cycle fields this module's doc header names. The engine
/// holds ONE of these. The in-cycle stance is gone — every begin
/// issues the detached arm.
/// Lock order: the engine mutex (which guards this whole struct) is the
/// OUTER lock; [`Self::outcome_ledger`]'s mutex is always an inner lock —
/// never the reverse (no ABBA ordering). The gauge atomics are lock-free.
pub(crate) struct DetachedCycle {
    /// THE merge pipe (see [`MergePipe`]): its two channel halves ARE the
    /// state, so the pipe's progress is never inferred from an `Option`
    /// combination.
    pipe: MergePipe,
    /// Monotonic counter bumped per issued detached SOLVE cycle (it is THE
    /// ledger's seq half). The sidecar's straggler-age telemetry reads
    /// `detached_issued_seq` against it.
    solve_seq_ctr: u64,
    /// The seq of the most recently issued detached cycle.
    detached_issued_seq: u64,
    /// Note-(a) carry: the duplicate-outcome fuse counter (the
    /// exactness assert). The sidecar's ledger claims feed it — one
    /// process-cumulative count.
    pub(crate) duplicate_outcomes: std::sync::atomic::AtomicU64,
    /// THE exactness ledger: one outcome
    /// per (`solve_seq`, path) EXACTLY once — keyed (`solve_seq`, pid); the
    /// sidecar claims under the enqueue-stamped `cycle_seq`. Held on the
    /// ENGINE (`parking_lot` Mutex) — the fuse is stateful across sidecar
    /// restarts (the pipe outlives any one sidecar thread).
    /// the machine OWNS and drives the field (via [`Self::claim`]);
    /// the TYPE stays `executor::outcome_ledger::OutcomeLedger`.
    pub(crate) outcome_ledger: parking_lot::Mutex<OutcomeLedger>,
    /// In-flight gauge: detached results SENT but not yet dispositioned.
    /// `Arc` because the enqueue half's bin threads bump it at send time
    /// ([`Self::gauge_hook`]) and the sidecar decrements it per Solved
    /// receipt ([`Self::solved_received`]). The gauge feeds the
    /// admission draw (`budget = max(0, admission_target_depth − in-flight)`),
    /// which is the sole backpressure now.
    pub(crate) outstanding: Arc<std::sync::atomic::AtomicU64>,
    /// Detached straggler outcome counters (applied / stale-dropped /
    /// deregistered-dropped); T2 wires the `detached.*` metrics from these.
    pub(crate) applied: std::sync::atomic::AtomicU64,
    pub(crate) dropped_stale: std::sync::atomic::AtomicU64,
    pub(crate) dropped_deregistered: std::sync::atomic::AtomicU64,
    /// Cycles SHED by capacity-modulated admission (zero draw
    /// budget) — the machine's disposition counter behind
    /// `degenbot.detached.shed`. A shed cycle submits nothing and
    /// claims nothing (no seq tick); this is a pure counter event.
    pub(crate) shed_cycles: std::sync::atomic::AtomicU64,
    /// Retained (carried) admission keys pruned by the retention
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
            pipe: MergePipe::Unopened,
            solve_seq_ctr: 0,
            detached_issued_seq: 0,
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
    /// The merge pipe's current state — the production verb reads it to keep a
    /// dead pipe's continued loss visible, and the conformance walk drives it.
    #[must_use]
    pub(crate) fn pipe_state(&self) -> MergePipeState {
        self.pipe.state()
    }
    /// The most recently issued detached cycle's seq — the sidecar's
    /// straggler-age telemetry anchor (`detached_issued_seq`).
    #[must_use]
    pub(crate) fn issued_seq(&self) -> u64 {
        self.detached_issued_seq
    }
    /// THE arm decision (one machine verb): tick the ONE seq counter, open the
    /// merge pipe once, and hand back the `Sender` clone for the 'static bin
    /// threads. This ALWAYS issues the detached arm — the admission draw owns
    /// backpressure — and the row is total: the machine cannot reject a cycle.
    ///
    /// A DEAD pipe still yields the sender it holds: the cycle submits as usual,
    /// every enqueue's send fails, and the lane's send-failure hook lands each
    /// stranded outcome on the drain-death accounting. The process lives.
    pub(crate) fn begin_cycle(&mut self) -> DetachedArm {
        // THE DETACHED ISSUE: tick the ONE counter and stamp the
        // detached-only telemetry anchor. The seq is per-ITEM bookkeeping and
        // stays orthogonal to the pipe's state.
        self.solve_seq_ctr += 1;
        self.detached_issued_seq = self.solve_seq_ctr;
        // The pipe: the FIRST begin opens the channel (one parked `Receiver`
        // for the sidecar spawner) and every later begin reuses it. The sidecar
        // thread is spawned by EngineStages::run_solve_cycle right after this
        // enqueue half returns.
        let (pipe, merge_tx) = std::mem::take(&mut self.pipe).begin();
        self.pipe = pipe;
        // Continued loss keeps the terminal state continuously visible: a
        // once-only line an operator can miss is not acceptable (the same
        // discipline as the throttled drain-death log).
        if self.pipe_state() == MergePipeState::Dead {
            op_debug!(
                domain = solver,
                cycle_seq = self.detached_issued_seq,
                "detached begin on a DEAD merge pipe — every outcome this cycle is a counted loss"
            );
        }
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
        crate::instruments::record(crate::instruments::CensusEvent::MergeInFlight {
            count: outstanding_now,
        });
        hotpath::gauge!("detached_solve_in_flight").set(f64::from(
            u32::try_from(outstanding_now).unwrap_or(u32::MAX),
        ));
        outstanding_now
    }
    /// Publish the in-flight gauge to both meters (the enqueue half's
    /// post-submit read).
    pub(crate) fn publish_gauge(&self) {
        let outstanding_now = self.outstanding.load(Ordering::Relaxed);
        crate::instruments::record(crate::instruments::CensusEvent::MergeInFlight {
            count: outstanding_now,
        });
        hotpath::gauge!("detached_solve_in_flight").set(f64::from(
            u32::try_from(outstanding_now).unwrap_or(u32::MAX),
        ));
    }
    /// One admission SHED cycle (zero draw budget) — the machine
    /// owns the disposition counter + its pipeline meter, mirroring
    /// [`Self::disposition`]. A shed cycle takes NO transition beyond the
    /// begin (the machine is not consulted at all on the deterministic path);
    /// this is a pure counter event.
    pub(crate) fn shed(&self) {
        self.shed_cycles.fetch_add(1, Ordering::Relaxed);
        crate::instruments::record(crate::instruments::CensusEvent::CycleShed);
    }
    /// `n` retained admission keys expired by the retention window
    /// (`head − W`) on a block advance — the machine counter + its pipeline
    /// meter. A zero count is a no-op (no spurious series touch).
    pub(crate) fn note_leads_expired(&self, n: usize) {
        let expired = u64::try_from(n).unwrap_or(u64::MAX);
        if expired == 0 {
            return;
        }
        self.leads_expired.fetch_add(expired, Ordering::Relaxed);
        crate::instruments::record(crate::instruments::CensusEvent::AdmissionKeysExpired {
            n: expired,
        });
    }
    /// ONE terminal disposition: land it on the machine's counter (+ the
    /// pipeline meter it feeds). The per-item LOG LINES stay at the call
    /// sites (they carry item fields — path id, seq, age — and their span
    /// parents).
    pub(crate) fn disposition(&self, kind: Disposition) {
        match kind {
            Disposition::Applied => {
                self.applied.fetch_add(1, Ordering::Relaxed);
                crate::instruments::record(crate::instruments::CensusEvent::StragglerApplied);
            }
            Disposition::DroppedStale => {
                self.dropped_stale.fetch_add(1, Ordering::Relaxed);
                crate::instruments::record(crate::instruments::CensusEvent::StragglerDropped);
            }
            Disposition::DroppedDeregistered => {
                self.dropped_deregistered.fetch_add(1, Ordering::Relaxed);
                crate::instruments::record(crate::instruments::CensusEvent::StragglerDropped);
            }
            Disposition::Duplicate => {
                self.duplicate_outcomes.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    /// THE one ledger door: the machine drives the ledger — every
    /// arm's claim runs through here with the machine-issued seq half.
    /// Callers hold the engine mutex across `claim` (the ledger mutex is
    /// always an inner lock — never the reverse: no ABBA ordering; see the
    /// verbatim note on `OutcomeLedger::claim`).
    pub(crate) fn claim(&self, k: (u64, u64)) -> Result<(), (u64, u64)> {
        self.outcome_ledger.lock().claim(k)
    }
    /// Hand the parked merge-pipe Receiver to the spawner:
    /// `EngineStages::run_solve_cycle` takes it ONCE, at the FIRST detached
    /// enqueue, and owns it inside the sidecar thread. Parked-only: `None` = the
    /// sidecar already took it, the pipe is dead, or no detached cycle ever
    /// enqueued.
    pub(crate) fn take_merge_rx(&mut self) -> Option<std::sync::mpsc::Receiver<LaneOutcome>> {
        let (pipe, taken) = std::mem::take(&mut self.pipe).take();
        self.pipe = pipe;
        taken
    }
    /// Record the sidecar's TERMINAL exit — the pipe's ONE Dead fact. The only
    /// consumer is gone, so no later delivery can land. Reports whether THIS
    /// report was the transition: a second report on the dead pipe changes
    /// nothing, and a pipe whose receiver was never taken has no sidecar.
    pub(crate) fn note_sidecar_exited(&mut self) -> bool {
        let was = self.pipe_state();
        self.pipe = std::mem::take(&mut self.pipe).sidecar_exited();
        self.pipe_state() != was
    }
}
// ---------------------------------------------------------------------------
// THE ONE sidecar spawn: both former engine_stages spawn sites
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
// DETACHED SOLVE CYCLE
// ---------------------------------------------------------------------------
// DETACH-ALWAYS (design locked 2026-09-02; the cutover retired the
// in-cycle arm so this is now the unconditional shape): the whole solve
// cycle RETURNS at ENQUEUE end — every result then flows through an
// UNBOUNDED mpsc to the merge sidecar, a plain `std::thread` (see the
// DEADLOCK note: a JOINING scope (a scoped rayon install of old, or
// against a held `parking_lot` guard; `std::thread` cannot deadlock with
// impatient pool) would starve against the Mutex; the sidecar cannot. The Q1a stale policy
// (apply-if-unchanged / drop-on-touched) makes the enqueue-time per-hop
// `update_block` snapshot a complete staleness oracle: a price-neutral
// liquidity event (V3 Mint/Burn, V4 ModifyLiquidity) advances the pool
// clock AND re-solves the path, so any stamp mismatch at merge time means
// the straggler's intake is stale and the result is DROPPED, never applied.
// This gate is now the SOLE staleness guard on the solve path (the ADR-021
// in-process solver-state tripwire is retired; only the upstream
// RPC-disagreement check survives at the Published edge). The in-flight
// cap constant lives with the cap consult in the one detached-cycle
// machine — `detached_cycle::DETACHED_INFLIGHT_CAP`.
// The detached-merge CARRIER is `executor::LaneOutcome`:
// the former single-variant enum folded into the unified
// `LaneOutcome::Solved(SolveOutcome)` — the typed `Solved`/`Suppressed`/
// `Failed` records both solve arms deliver (the sidecar's
// pipe now also carries the lane witness's `Failed` panic records). The
// exactness ledger age moved with it (`executor::outcome_ledger::LEDGER_AGE`,
// carried unchanged).
/// The detached-merge SIDECAR thread body: owns the
/// unbounded mpsc `Receiver` of the merge pipe and applies each item under
/// the engine Mutex — Q1a stale gate + the SAME merge/emit path as the
/// in-cycle drain (`merge_one_result`, which carries the streaming
/// delivery emission). Spawned by `EngineStages::run_solve_cycle` at the FIRST
/// detached enqueue; runs until every `Sender` drops (engine teardown),
/// so the pipe never strands items across the engine's lifetime. Whatever ends
/// it — a caught merge panic, or the channel closing — its terminal exit records
/// the pipe's ONE [`MergePipe::Dead`] fact on the machine before the thread
/// returns.
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
            // a panicking merge must NEVER silently kill this
            // thread — that drops the Receiver and strands every later
            // send with no signal. catch_unwind converts the panic into
            // the SAME typed drain-death terminal state as a failed send
            // (sticky cordon + counter + loud log); the process lives.
            let merged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // the terminated member now chains machine-direct
                // (the engine-level `merge_detached_item` casing is gone).
                // ONE engine acquisition per item; the machine's own drain
                // never locks for itself.
                let mut guard = engine.lock();
                let e: &mut ArbitrageEngine = &mut guard;
                e.cycle
                    .merge_detached_item(item, &e.registry, &mut e.delivery);
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
                break;
            }
        }
        // Terminal exit: the pipe's only consumer is gone for good, so no later
        // send can land. Record the ONE Dead fact here — the outcomes themselves
        // are counted per item by the lane's send-failure hook, never guessed
        // from this transition.
        let mut guard = engine.lock();
        if guard.cycle.detached_cycle.note_sidecar_exited() {
            op_debug!(
                domain = solver,
                "detached merge sidecar exited — the merge pipe is DEAD (every later outcome is a counted loss)"
            );
        }
    });
}
/// Spawn the detached merge sidecar for the parked receiver. The take-once
/// is [`DetachedCycle::take_merge_rx`], done by the
/// caller under whatever engine hold it already owns; this fn registers the
/// pinned seat and starts the named thread. A spawn failure LOUDLY ABORTS:
/// a stranded merge pipe would silently orphan every detached result
/// (the worker-fleet deadlock ledger, `docs/architecture/worker-fleet.md`
/// §10 — wording + abort verbatim).
pub(crate) fn spawn_merge_sidecar(
    engine: &std::sync::Arc<parking_lot::Mutex<super::ArbitrageEngine>>,
    merge_rx: std::sync::mpsc::Receiver<LaneOutcome>,
) {
    let engine_arc = std::sync::Arc::clone(engine);
    // self-register the pinned merge sidecar (the fleet
    // Merge role — the only posture since the LW-T9 cutover).
    degenbot_core::worker_census::register(merge_sidecar_census_entry());
    if let Err(err) = std::thread::Builder::new()
        .name(merge_sidecar_thread_name())
        .spawn(move || {
            // production uses the process posture owner (None).
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
    /// The sized ALL-states const is exhaustive and duplicate-free; adding a
    /// state without extending the table + this walk fails the exhaustive match
    /// below at compile time (house pattern: `stage_handlers::ALL_STAGES` /
    /// `slot.rs` `ALL_ROLES`).
    #[test]
    fn all_merge_pipe_states_covers_every_state_exactly_once() {
        assert_eq!(ALL_MERGE_PIPE_STATES.len(), 4, "the pipe declares 4 states");
        for (i, s) in ALL_MERGE_PIPE_STATES.iter().enumerate() {
            assert!(
                !ALL_MERGE_PIPE_STATES[i + 1..].contains(s),
                "duplicate state {s:?} in ALL_MERGE_PIPE_STATES"
            );
        }
        for s in ALL_MERGE_PIPE_STATES {
            // Exhaustive: a new variant breaks this match at compile time.
            match s {
                MergePipeState::Unopened
                | MergePipeState::Parked
                | MergePipeState::Live
                | MergePipeState::Dead => {}
            }
        }
        assert_eq!(
            ALL_MERGE_PIPE_STATES,
            [
                MergePipeState::Unopened,
                MergePipeState::Parked,
                MergePipeState::Live,
                MergePipeState::Dead,
            ]
        );
    }
    /// A real machine driven into `state` — the walk's setup, through the
    /// machine's own verbs rather than a fabricated field.
    fn machine_in(state: MergePipeState) -> DetachedCycle {
        let mut m = DetachedCycle::new();
        if state == MergePipeState::Unopened {
            return m;
        }
        let _arm = m.begin_cycle();
        assert_eq!(m.pipe_state(), MergePipeState::Parked);
        if state == MergePipeState::Parked {
            return m;
        }
        let _rx = m.take_merge_rx().expect("the parked receiver is take-once");
        assert_eq!(m.pipe_state(), MergePipeState::Live);
        if state == MergePipeState::Live {
            return m;
        }
        m.note_sidecar_exited();
        assert_eq!(m.pipe_state(), MergePipeState::Dead);
        m
    }
    /// THE CONFORMANCE WALK: every (state × event) cell lands on its table
    /// successor, and the MACHINE's verbs answer exactly the table's successor —
    /// the table cannot drift from the code it documents.
    #[test]
    fn conformance_walks_every_legal_transition() {
        for state in ALL_MERGE_PIPE_STATES {
            for event in [
                MergePipeEvent::Begin,
                MergePipeEvent::Take,
                MergePipeEvent::SidecarExited,
            ] {
                let expected = transition(state, event);
                let mut machine = machine_in(state);
                match event {
                    MergePipeEvent::Begin => {
                        let arm = machine.begin_cycle();
                        assert_eq!(
                            arm.cycle_seq,
                            machine.issued_seq(),
                            "a begin stamps its own seq tick ({state:?})"
                        );
                    }
                    MergePipeEvent::Take => {
                        assert_eq!(
                            machine.take_merge_rx().is_some(),
                            state == MergePipeState::Parked,
                            "a take hands the receiver out on the Parked row only ({state:?})"
                        );
                    }
                    MergePipeEvent::SidecarExited => {
                        assert_eq!(
                            machine.note_sidecar_exited(),
                            state == MergePipeState::Live,
                            "the Dead fact is recorded on the Live → Dead row only ({state:?})"
                        );
                    }
                }
                assert_eq!(
                    machine.pipe_state(),
                    expected,
                    "{state:?} + {event:?} must land on the table's successor"
                );
            }
        }
    }
    // ---- transition tests: the drivers, not the table ----
    /// Repeated begin: the pipe opens once. Both cycles' senders deliver into
    /// the ONE parked receiver, so a single drain serves every cycle while the
    /// seq keeps ticking per cycle (per-item bookkeeping, orthogonal to the
    /// pipe).
    #[test]
    fn repeated_begin_opens_the_pipe_once() {
        let mut m = DetachedCycle::new();
        let first = m.begin_cycle();
        assert_eq!(m.pipe_state(), MergePipeState::Parked);
        let second = m.begin_cycle();
        assert_eq!(
            m.pipe_state(),
            MergePipeState::Parked,
            "a second begin reuses the parked pipe (no re-open)"
        );
        assert_eq!(
            (first.cycle_seq, second.cycle_seq),
            (1, 2),
            "the ONE seq counter ticks per issued cycle"
        );
        let rx = m
            .take_merge_rx()
            .expect("the receiver parks for the spawner");
        first
            .merge_tx
            .send(LaneOutcome::Suppressed { pid: 1 })
            .expect("the first cycle's sender reaches the one pipe");
        second
            .merge_tx
            .send(LaneOutcome::Suppressed { pid: 2 })
            .expect("the second cycle's sender reaches the SAME pipe");
        assert_eq!(rx.recv().expect("one drain serves both cycles").pid(), 1);
        assert_eq!(rx.recv().expect("one drain serves both cycles").pid(), 2);
        assert_eq!(m.pipe_state(), MergePipeState::Live);
    }
    /// Double take: the receiver is handed out exactly once. The spawn check
    /// runs after every cycle, so a second take finds nothing to spawn and the
    /// drain can never fork.
    #[test]
    fn receiver_take_is_parked_only() {
        let mut m = DetachedCycle::new();
        assert!(
            m.take_merge_rx().is_none(),
            "an unopened pipe has no receiver to take"
        );
        let _arm = m.begin_cycle();
        let parked = m.take_merge_rx().expect("the parked receiver is take-once");
        assert_eq!(m.pipe_state(), MergePipeState::Live);
        assert!(
            m.take_merge_rx().is_none(),
            "a second take must not fork the drain"
        );
        drop(parked);
    }
    /// Sidecar death: the terminal exit records one Dead fact, and the pipe
    /// stays dead through every later report and take.
    #[test]
    fn sidecar_exit_records_one_dead_fact() {
        let mut m = DetachedCycle::new();
        let _arm = m.begin_cycle();
        let rx = m
            .take_merge_rx()
            .expect("the sidecar takes the parked receiver");
        assert_eq!(m.pipe_state(), MergePipeState::Live);
        assert!(m.note_sidecar_exited(), "Live → Dead is the one transition");
        assert_eq!(m.pipe_state(), MergePipeState::Dead);
        assert!(
            !m.note_sidecar_exited(),
            "a second exit report changes nothing: the Dead fact is one"
        );
        assert!(
            m.take_merge_rx().is_none(),
            "a dead pipe has no receiver left to take"
        );
        assert_eq!(m.pipe_state(), MergePipeState::Dead, "…and stays dead");
        drop(rx);
    }
    /// Later cycles on a dead pipe: the begin still issues (the lane witness
    /// still owes one outcome per submitted path) and hands out the dead
    /// channel's sender, so each enqueue's send fails and the lane's
    /// send-failure hook counts the loss — no refusal, no abort.
    #[test]
    fn a_later_cycle_on_a_dead_pipe_sends_into_the_dead_channel() {
        let mut m = DetachedCycle::new();
        let _first = m.begin_cycle();
        let rx = m
            .take_merge_rx()
            .expect("the sidecar takes the parked receiver");
        drop(rx);
        assert!(m.note_sidecar_exited(), "the sidecar exited for good");
        assert_eq!(m.pipe_state(), MergePipeState::Dead);
        let later = m.begin_cycle();
        assert_eq!(later.cycle_seq, 2, "a dead pipe still ticks the seq");
        assert_eq!(
            m.pipe_state(),
            MergePipeState::Dead,
            "a later begin leaves the dead pipe dead"
        );
        assert!(
            later
                .merge_tx
                .send(LaneOutcome::Suppressed { pid: 9 })
                .is_err(),
            "the dead channel refuses delivery — the lane's send-failure hook owns the accounting"
        );
    }
    /// the begin ALWAYS issues the detached arm — a cycle whose
    /// in-flight gauge sits at/over the design depth safety valve still
    /// detaches (the admission draw, not a cap verdict, owns backpressure).
    #[test]
    fn machine_begins_detached_even_at_or_over_the_depth_safety_valve() {
        let mut m = DetachedCycle::new();
        for outstanding in [0, DETACHED_INFLIGHT_CAP, DETACHED_INFLIGHT_CAP + 5] {
            m.outstanding.store(outstanding, Ordering::Relaxed);
            let DetachedArm { .. } = m.begin_cycle();
            assert_eq!(m.pipe_state(), MergePipeState::Parked);
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
        // EXACT: two bumps and two receipts leave NOTHING pending — the gauge
        // the admission draw reads never drifts from the real in-flight set.
        assert_eq!(m.solved_received(), 0);
        assert_eq!(m.outstanding.load(Ordering::Relaxed), 0);
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
