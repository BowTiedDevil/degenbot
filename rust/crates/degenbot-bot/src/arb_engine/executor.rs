//! THE Executor seam (parking-lot decision, LW-T8 JI275C): the name is
//! **`Executor`** — the LANEWARDEN vocabulary finalizes here. Since LNQDOA
//! (the pooled intake port) this module owns the GLOBAL EXECUTOR TOKEN
//! FAMILY for `arb_engine` — `global_executor` (the solve-arm seam) plus
//! the pooled-intake delegate `global_sim_executor` (which hands out
//! `fleet_intake`'s `FleetIntake` port over the pooled sim executor; the
//! registration intake arm is served by the `fleet_intake` facade's
//! `registration_intake`) — and re-exports the shared
//! seam types (mirroring the degenbot-workers placement of shared types —
//! no pyo3 in any signature).

use degenbot_workers::dispatcher::{BootError, SubmitError};
use degenbot_workers::lane::LaneCtx;

pub(crate) use degenbot_workers::dispatcher::SubmitReceipt;

/// One escalated work item handed through a seat's `LaneCtx` port.
pub(crate) type SubmitWork = Box<dyn FnOnce(&LaneCtx) + Send + 'static>;

/// The ONE executor seam (LW-T8): `boot/bin_count/submit` + the
/// `LaneCtx`/`EscalationPort` contracts. (JCI2FW Part A: the retired
/// `observe_throttle` absorb-by-contract default is dissolved — the ONE
/// process posture owner (`degenbot_workers::posture::process`) is fed
/// by the block pump directly, and every fleet host consults the same
/// owner; the seam carries no posture channel anymore.)
pub(crate) trait Executor: Send + Sync {
    /// The structural seat count bins bind at (P6YXA6).
    fn bin_count(&self) -> usize;

    /// Submit one LPT bin job; the unit body receives the seat's `LaneCtx`.
    /// Admission is posture-invariant (7OGY5V/024ef513d): a posture refusal
    /// at the gate aborts LOUDLY (process exit) with the 'bin submission,
    /// posture gate' context; accepted units are never silently dropped
    /// (units ride one of the solved-arm lanes; receipts are tracked per
    /// lane).
    fn submit(&self, bin: usize, work: SubmitWork) -> Result<SubmitReceipt, SubmitError>;
}

/// The solve-arm global token (LW-T8): every SOLVE call site submits
/// through here. LNQDOA: it is the solve arm of a token FAMILY — the
/// pooled intake arms submit through `global_sim_executor` here (which
/// delegates to `fleet_intake`'s `FleetIntake` port) and
/// `fleet_intake::registration_intake` (the pyo3-leaf registration arm),
/// not through this fn — the fleet-hosted executors remain the only
/// executors since the LW-T9 cutover (the tokio stance is deleted; there
/// is no stance parameter).
/// YI5NGB construction precondition: the fleet materializes LAZILY on the
/// first call — from the construction-STAMPED boot the first engine
/// construction installed (`with_core_cfg`). There is NO fallback boot: a
/// caller that reaches this seam BEFORE any engine construction is a
/// contract violation and aborts LOUD (`expect` in the materializer).
pub(crate) fn global_executor() -> &'static dyn Executor {
    crate::arb_engine::fleet_solve_executor::global_fleet_solve_executor()
}

/// The pooled SIM intake delegate (LNQDOA): hands out `fleet_intake`'s
/// `FleetIntake` port over the fleet sim executor, so the sim dispatch
/// route reads through ONE module (the trait object flows in-crate only).
pub(crate) fn global_sim_executor(
) -> Result<&'static dyn crate::arb_engine::fleet_intake::FleetIntake, BootError> {
    crate::arb_engine::fleet_intake::sim_intake()
}
// ---------------------------------------------------------------------------
// THE solve lane (QR3NUS 43E3H3): the one outcome-carrier module both solve
// arms submit through. Folded here from the fleet solve executor's
// provisional lane module (JI275C) — the placement is the one JI275C named.
// WITNESS + CARRIER + LEDGER live together on purpose: the lane knows the
// pids it owes, the carrier carries what the merge consumes, and the ledger
// asserts the one-outcome-per-path-per-cycle law across BOTH arms.
// ---------------------------------------------------------------------------

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc;

use degenbot_workers::dispatcher::{PanicAction, PanicVerdict};

use crate::arb_engine::inline_sim::SimulatedPathResult;
use crate::arb_engine::{BlockMetadata, SolvePathResult};

/// Why one or more of a unit's paths never delivered an outcome to the
/// result pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneFailure {
    /// The bin closure panicked mid-unit; the seat survives (QR3NUS
    /// decision A) and every undelivered path becomes one of these.
    SeatPanic {
        /// The host-tracked unit id that panicked.
        unit: u64,
        /// The seat (slot) that was executing the unit.
        seat: u64,
        /// The panic payload when it is a string.
        message: Option<String>,
    },
    /// The LANE died mid-flight (FF-T4, Z6XTDX): the seat thread
    /// abandoned its bin before draining its paths — every still-owed
    /// path becomes one of these terminal records. The fleet cordons
    /// (the sticky [`degenbot_workers::posture::PostureCause::LaneDeath`]
    /// input) and the process LIVES: the terminal receipts keep the
    /// outcome ledger exact (solved + suppressed + failed == submitted),
    /// never a stranded submitter, never a silent undercount.
    LaneDeath {
        /// The host-tracked unit id whose lane died.
        unit: u64,
        /// The seat (slot) whose lane died.
        seat: u64,
    },
}

/// The unified solved payload: the solved arm's
/// `(pid, result, worker_clamp_twins, payload)` data PLUS the
/// issuing-cycle identity (the exactness-ledger key material + the Q1a
/// oracle) PLUS the per-merge `merge_one_result` arguments the sidecar
/// reads off the item. The detached arm fills `cycle_seq`/`update_stamp`/
/// `solve_span` per enqueue; the in-cycle arm fills them with inert values
/// its drain never reads (`cycle_seq` 0 — its claims use the cycle's own seq).
pub(crate) struct SolveOutcome {
    pub pid: u64,
    pub result: SolvePathResult,
    pub worker_clamp_twins: u64,
    pub payload: Option<SimulatedPathResult>,
    /// The solve block the result was computed against. The in-cycle drain
    /// asserts it equals its own cycle value (a mismatch is a wrong-arm
    /// delivery — loud in debug).
    pub solve_block: u64,
    /// Cycle metadata for the streaming emission (Copy).
    pub metadata: BlockMetadata,
    /// Issuing cycle's solve sequence (the ledger's cross-cycle key).
    /// 0 on the in-cycle arm (its claims stamp the cycle's own seq).
    pub cycle_seq: u64,
    /// Per-hop `pool_update_block` snapshot at the enqueue resolve — the Q1a
    /// staleness oracle. Empty on the in-cycle arm (its stamps are live by
    /// construction: the cycle holds the Mutex through the merge).
    pub update_stamp: Vec<u64>,
    /// The enqueue-time solve span the sidecar re-enters per item
    /// (MQUKB6-T2). `Span::none()` on the in-cycle arm and in tests.
    pub solve_span: tracing::Span,
}

/// One drained per-path outcome: EXACTLY one per submitted path.
#[expect(clippy::large_enum_variant)]
// Deliberate: the Solved arm carries the full merge payload inline
// (~560B) — Boxing it would add a per-solve-path heap hop on the hot
// path, and the enum's other arms are deliberately tiny (the accounting
// records are counters, not payloads). The solve arms are throughput-
// bound per seat, and the LaneOutcome is consumed eagerly by the drain
// (no long-lived enum storage), so the variant-size asymmetry is fine.
pub(crate) enum LaneOutcome {
    /// A real solve result, stamped with issuing-cycle identity.
    Solved(SolveOutcome),
    /// The worker's None arm (failed / filtered solve): never merges, but
    /// is still an outcome the accounting must count.
    Suppressed { pid: u64 },
    /// A path whose outcome never landed because its unit panicked.
    Failed { pid: u64, failure: LaneFailure },
}

impl LaneOutcome {
    /// The path id this outcome witnesses (every variant carries one).
    pub(crate) fn pid(&self) -> u64 {
        match self {
            Self::Solved(item) => item.pid,
            Self::Suppressed { pid } | Self::Failed { pid, .. } => *pid,
        }
    }
}

/// One typed terminal record for a DEAD MERGE DRAIN (AQV6EF): the merge
/// pipe's only consumer is gone, so no later outcome can ever be delivered.
/// Deliberately NOT a [`LaneFailure`]: a `LaneFailure` rides the (live)
/// pipe as a typed `Failed` record; this names the death of the pipe
/// ITSELF, so it can only be surfaced out-of-band — counter + sticky pose
/// cause + loud log — and must never be fabricated as a pipe delivery (the
/// exactness ledger is owed deliveries for SUBMITTED paths only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DrainFailure {
    /// A `LaneOutcome` send failed: the merge seat's Receiver is gone.
    SendFailed {
        /// The path whose outcome was lost.
        pid: u64,
        /// The submitting unit id.
        unit: u64,
        /// The submitting seat (slot).
        seat: u64,
    },
    /// `merge_detached_item` panicked while consuming an item; the
    /// sidecar's `catch_unwind` guard caught it.
    MergePanic {
        /// The panic payload when it is a string.
        message: Option<String>,
    },
    /// An outcome was still queued when the panicked sidecar shut down —
    /// drained and counted so that class is never a silent loss.
    Stranded {
        /// The path whose outcome was lost.
        pid: u64,
    },
}

/// The detached arm's drain-death hook (AQV6EF): `Arc`-shared so every
/// bin thread clones it; fired with the typed failure so the hook stays a
/// plain policy function (`drain_death_response`, or a test recorder).
pub(crate) type DrainDeathHook = std::sync::Arc<dyn Fn(&DrainFailure) + Send + Sync>;

/// The drain-death loud-log cadence (AQV6EF): EVERY loss is counted (the
/// metric and the posture cause), but the error line is emitted on the
/// first occurrence and then every `DRAIN_DEATH_LOG_EVERY`th, so a dead
/// pipe cannot flood the log while the terminal state stays continuously
/// VISIBLE — a once-only line an operator can miss is not acceptable.
const DRAIN_DEATH_LOG_EVERY: u64 = 256;
static DRAIN_DEATH_LOGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The ONE drain-death response (AQV6EF). The merge pipe's only consumer is
/// the sidecar thread, and `spawn_merge_sidecar` spawns exactly one per
/// engine lifetime — a dead merge seat can NEVER recover in-process.
/// CLASSIFICATION: a dead merge seat is NOT a `failure_policy`
/// Fatal/Exit bucket; it is the FF-T4 LANE-DEATH terminal class — a STICKY
/// cordon plus a live (loud) process, exactly like a solve-lane death.
/// `owner` overrides the process posture owner for hermetic tests.
pub(crate) fn drain_death_response(
    failure: &DrainFailure,
    owner: Option<&degenbot_workers::posture::PostureOwner>,
) {
    match failure {
        DrainFailure::SendFailed { .. } | DrainFailure::Stranded { .. } => {
            if let Some(p) = crate::instruments::pipeline() {
                p.count_detached_send_failed();
            }
        }
        DrainFailure::MergePanic { .. } => {
            if let Some(p) = crate::instruments::pipeline() {
                p.count_detached_merge_panic();
            }
        }
    }
    let lane_death_change = match owner {
        Some(owner) => owner.observe_cause(degenbot_workers::posture::PostureCause::LaneDeath),
        None => degenbot_workers::posture::process()
            .observe_cause(degenbot_workers::posture::PostureCause::LaneDeath),
    };
    // Feeder-site contract (T3): wake the fleet hosts on a real transition.
    if !matches!(
        lane_death_change,
        degenbot_workers::posture::PostureChange::Held
    ) {
        crate::arb_engine::fleet_wake::wake_hosts();
    }
    let occurrence = DRAIN_DEATH_LOGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if occurrence == 1 || occurrence.is_multiple_of(DRAIN_DEATH_LOG_EVERY) {
        tracing::error!(
            target: "degenbot::fleet",
            failure = ?failure,
            occurrence,
            "[fleet-solve] merge drain DEAD — outcome lost and counted, sticky posture cordon set (FF-T4 lane death); the process lives, only a fresh process lifts it (AQV6EF)"
        );
    }
}

/// The lane WITNESS for one bin: it owes the pipe exactly one outcome per
/// submitted pid — delivered ones as they happen, undelivered ones patched
/// as typed `Failed` records after a panic (the seat survives).
pub(crate) struct SolveLane {
    unit: u64,
    seat: u64,
    pids: Vec<u64>,
    emitted: BTreeSet<u64>,
    tx: mpsc::Sender<LaneOutcome>,
    /// 43E3H3: the DETACHED arm's in-flight gauge hook, fired on a
    /// `Solved` item's SEND SUCCESS only (never for Suppressed/Failed —
    /// REV 2 Defect 1: those never bump, so they may never decrement).
    /// `None` on the in-cycle arm (which has no in-flight gauge).
    on_solved_send: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// AQV6EF: the DETACHED arm's drain-death hook, fired on a terminal
    /// `LaneOutcome` send FAILURE (the merge pipe's Receiver is gone).
    /// Strictly additive: the failed send is NOT re-delivered and the
    /// in-flight gauge is NOT bumped — the ledger is owed deliveries for
    /// SUBMITTED paths only, so this surfaces the DRAIN DEATH instead of
    /// fabricating a pipe delivery. `None` on the in-cycle arm.
    on_send_failed: Option<DrainDeathHook>,
}

impl SolveLane {
    /// Build a lane for one bin: `unit`/`seat` name the accounting identity
    /// the panic records will carry; `pids` are the paths the bin's work
    /// owed the pipe.
    pub(crate) fn new(unit: u64, seat: u64, pids: Vec<u64>, tx: mpsc::Sender<LaneOutcome>) -> Self {
        Self {
            unit,
            seat,
            pids,
            emitted: BTreeSet::new(),
            tx,
            on_solved_send: None,
            on_send_failed: None,
        }
    }

    /// Install the detached arm's gauge hook (fired on `Solved` send
    /// success). MUST be called before `run_solve_lane` drives the bin.
    pub(crate) fn set_on_solved_send(
        &mut self,
        on_solved_send: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) {
        self.on_solved_send = Some(on_solved_send);
    }

    /// Install the detached arm's drain-death hook (fired when a terminal
    /// send fails). MUST be called before `run_solve_lane` drives the bin.
    pub(crate) fn set_on_send_failed(&mut self, on_send_failed: DrainDeathHook) {
        self.on_send_failed = Some(on_send_failed);
    }

    /// Deliver one real arm outcome (the worker's `Some` arm). The lane's
    /// `emitted` set is the DOUBLE-DELIVERY guard: every pid released this
    /// way is excluded from the post-panic patch, so a flushed
    /// outcome is never ALSO patched as Failed (the over-disposition the
    /// breaker suite catches).
    pub(crate) fn solved(&mut self, item: SolveOutcome) {
        self.emitted.insert(item.pid);
        let pid = item.pid;
        if self.tx.send(LaneOutcome::Solved(item)).is_ok() {
            if let Some(gauge) = self.on_solved_send.as_ref() {
                gauge();
            }
        } else {
            self.note_send_failed(pid);
        }
    }

    /// Deliver the worker's `None` arm — still an outcome (counted).
    pub(crate) fn suppressed(&mut self, pid: u64) {
        self.emitted.insert(pid);
        if self.tx.send(LaneOutcome::Suppressed { pid }).is_err() {
            self.note_send_failed(pid);
        }
    }

    /// Patch one typed per-path failure onto the pipe (decision A):
    /// exactly one outcome for `pid`, carrying unit + seat.
    pub(crate) fn failed(&mut self, pid: u64, failure: LaneFailure) {
        self.emitted.insert(pid);
        if self.tx.send(LaneOutcome::Failed { pid, failure }).is_err() {
            self.note_send_failed(pid);
        }
    }

    /// AQV6EF: surface a terminal send failure that would otherwise be
    /// swallowed. Strictly additive — no pipe delivery is fabricated (the
    /// pid is already in `emitted`, the double-delivery guard) and no
    /// gauge is touched; the hook is the detached arm's drain-death signal.
    fn note_send_failed(&self, pid: u64) {
        if let Some(hook) = self.on_send_failed.as_ref() {
            hook(&DrainFailure::SendFailed {
                pid,
                unit: self.unit,
                seat: self.seat,
            });
        }
    }

    /// The pids this bin still owed the pipe.
    pub(crate) fn unemitted(&self) -> Vec<u64> {
        self.pids
            .iter()
            .copied()
            .filter(|pid| !self.emitted.contains(pid))
            .collect()
    }
}

/// The lane-death response (FF-T4, Z6XTDX — AC 3): a lane that died
/// mid-flight (its seat thread abandoned the bin before draining its
/// paths) gets TERMINAL RECEIPTS — every still-owed path patched onto
/// the pipe as exactly one typed `Failed(LaneFailure::LaneDeath)`
/// record — the cordoned posture (the sticky `PostureCause::LaneDeath`
/// input; `owner` overrides the process owner for hermetic tests), and
/// a LIVE process: this never aborts. Returns the patched record count.
pub(crate) fn lane_death_response(
    lane: &mut SolveLane,
    owner: Option<&degenbot_workers::posture::PostureOwner>,
) -> usize {
    let unemitted = lane.unemitted();
    let patched = unemitted.len();
    let unit = lane.unit;
    let seat = lane.seat;
    for pid in unemitted {
        lane.failed(pid, LaneFailure::LaneDeath { unit, seat });
    }
    let lane_death_change = match owner {
        Some(owner) => owner.observe_cause(degenbot_workers::posture::PostureCause::LaneDeath),
        None => degenbot_workers::posture::process()
            .observe_cause(degenbot_workers::posture::PostureCause::LaneDeath),
    };
    // Feeder-site contract (T3): wake the fleet hosts on a real transition.
    if !matches!(
        lane_death_change,
        degenbot_workers::posture::PostureChange::Held
    ) {
        crate::arb_engine::fleet_wake::wake_hosts();
    }
    tracing::error!(
        target: "degenbot::fleet",
        unit,
        seat,
        patched,
        "[fleet-solve] lane died mid-flight — terminal failure records patched, posture cordoned (sticky), the process lives (FF-T4)"
    );
    patched
}

/// Drive one bin body under the lane witness: a panic is caught (the seat
/// backstop also survives), the `PanicVerdict` is consulted, and every
/// still-unemitted path is patched onto the pipe as exactly one typed
/// `Failed(LaneFailure::SeatPanic)` record — so BOTH arms satisfy
/// "one outcome per submitted path" even through a panic. A bin body
/// that RETURNS with still-owed paths is a lane death (the seat
/// abandoned its bin mid-flight, FF-T4): the lane-death response fires —
/// terminal receipts + the cordoned posture + a live process — instead of
/// the silent sender-drop undercount that used to trip the merge fuse.
pub(crate) fn run_solve_lane(
    lane: &mut SolveLane,
    verdict: &dyn PanicVerdict,
    work: impl FnOnce(&mut SolveLane),
) {
    let outcome = AssertUnwindSafe(|| work(lane));
    let outcome = std::panic::catch_unwind(outcome);
    let Err(payload) = outcome else {
        if !lane.unemitted().is_empty() {
            lane_death_response(lane, None);
        }
        return;
    };
    let message = if let Some(text) = payload.downcast_ref::<&str>() {
        Some((*text).to_owned())
    } else {
        payload.downcast_ref::<String>().cloned()
    };
    match verdict.on_unit_panic(lane.unit, lane.seat) {
        PanicAction::RecordAndContinue => {
            let unemitted = lane.unemitted();
            let patched = unemitted.len();
            for pid in unemitted {
                lane.failed(
                    pid,
                    LaneFailure::SeatPanic {
                        unit: lane.unit,
                        seat: lane.seat,
                        message: message.clone(),
                    },
                );
            }
            tracing::error!(
                target: "degenbot::fleet",
                unit = lane.unit,
                seat = lane.seat,
                message = ?message,
                patched,
                "[fleet-solve] bin job panicked — seat survives with typed failure records (QR3NUS decision A)"
            );
        }
        PanicAction::Abort => {
            crate::arb_engine::fleet_solve_executor::abort_executor(
                "lane panic verdict: Abort",
                &message.unwrap_or_default(),
            );
        }
    }
}

/// THE outcome ledger (QR3NUS 43E3H3): ONE implementation asserting one
/// typed outcome per path per cycle exactly once, across BOTH solve arms
/// (the in-cycle drain and the detached merge sidecar). Keyed
/// `(solve_seq, pid)`; the prune-age constant is carried unchanged from
/// the former sidecar ledger age (LW-T9 note (a)).
pub(crate) mod outcome_ledger {
    /// How many recent solve cycles the ledger spans before pruning
    /// (carried unchanged from the former sidecar ledger age constant;
    /// the boot
    /// in-flight cap bounds meaningful straggler age at ~8, so 64 keeps a
    /// duplicate permanently longer than any straggler can live — the
    /// LW-T9 note (a) invariant, now uniform across both arms).
    pub(crate) const LEDGER_AGE: u64 = 64;

    /// The seen-outcome ledger. One key spelling for BOTH arms:
    /// `(solve_seq, pid)`. The seq comes from the engine's monotone
    /// `solve_seq_ctr` (both arms tick it). Pruning anchors on the CURRENT
    /// claim's seq.
    #[derive(Default)]
    pub(crate) struct OutcomeLedger {
        seen: std::collections::HashSet<(u64, u64)>,
    }

    impl OutcomeLedger {
        /// ONE check-and-claim: prune older cycles, then claim `(seq, pid)`.
        /// `Ok(())` = first sighting; `Err(k)` = duplicate — the fuse.
        /// Callers hold the engine mutex across `claim` (the ledger mutex is
        /// always an inner lock — never the reverse: no ABBA ordering).
        pub(crate) fn claim(&mut self, k: (u64, u64)) -> Result<(), (u64, u64)> {
            self.prune(k.0);
            if self.seen.contains(&k) {
                return Err(k);
            }
            self.seen.insert(k);
            Ok(())
        }

        /// Direct row membership — the tests' ledger-inspect surface
        /// (production code paths use [`claim`] only).
        #[cfg(test)]
        pub(crate) fn contains(&self, k: (u64, u64)) -> bool {
            self.seen.contains(&k)
        }

        fn prune(&mut self, cycle_seq: u64) {
            self.seen
                .retain(|(seq, _)| *seq >= cycle_seq.saturating_sub(LEDGER_AGE));
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use degenbot_solvers::mixed::SolvePathResult;
    use degenbot_workers::posture::{FleetPosture, PostureOwner, PosturePolicy, ThrottleSample};

    fn solved(pid: u64) -> SolveOutcome {
        SolveOutcome {
            pid,
            result: SolvePathResult::default(),
            worker_clamp_twins: 0,
            payload: None,
            solve_block: 0,
            metadata: crate::arb_engine::BlockMetadata::default(),
            cycle_seq: 1,
            update_stamp: Vec::new(),
            solve_span: tracing::Span::none(),
        }
    }

    fn hermetic_owner() -> &'static PostureOwner {
        std::boxed::Box::leak(std::boxed::Box::new(PostureOwner::new(
            PosturePolicy::doc_defaults(),
        )))
    }

    /// AQV6EF AC1 (red-first): an outcome send against a DROPPED merge
    /// Receiver must fire the drain-death hook with the typed failure —
    /// and must NOT bump the in-flight gauge (the failed send was never a
    /// delivery).
    #[test]
    fn a_send_against_a_dropped_receiver_fires_the_drain_death_hook() {
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        drop(rx); // the merge seat is gone
        let seen: Arc<StdMutex<Vec<DrainFailure>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_hook = Arc::clone(&seen);
        let gauge_calls = Arc::new(AtomicU64::new(0));
        let gauge_hook = Arc::clone(&gauge_calls);
        let mut lane = SolveLane::new(7, 3, vec![11], tx);
        lane.set_on_solved_send(Arc::new(move || {
            gauge_hook.fetch_add(1, Ordering::Relaxed);
        }));
        lane.set_on_send_failed(Arc::new(move |failure| {
            seen_hook.lock().expect("hook mutex").push(failure.clone());
        }));
        lane.solved(solved(11));
        let failures = seen.lock().expect("hook mutex");
        assert_eq!(
            failures.as_slice(),
            &[DrainFailure::SendFailed {
                pid: 11,
                unit: 7,
                seat: 3
            }],
            "the swallowed send must surface as a typed drain failure"
        );
        assert_eq!(
            gauge_calls.load(Ordering::Relaxed),
            0,
            "a FAILED send is not a delivery: the in-flight gauge must not bump"
        );
    }

    /// AQV6EF AC1/AC2: the drain-death response trips the FF-T4 STICKY
    /// cordon (only a fresh process lifts it) and stays loud.
    #[test]
    fn a_dead_merge_drain_cordons_the_fleet_stickily() {
        let owner = hermetic_owner();
        drain_death_response(
            &DrainFailure::MergePanic {
                message: Some("merge boom".to_owned()),
            },
            Some(owner),
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        for i in 0..30_u64 {
            owner.observe_throttle(
                10_000 + i * 1_000,
                ThrottleSample {
                    events: 0,
                    throttled_usec: 0,
                    elapsed_usec: 1_000_000,
                },
            );
        }
        assert_eq!(
            owner.current(),
            FleetPosture::Cordoned,
            "a merge-drain death cordon is sticky across clean windows"
        );
    }
}
