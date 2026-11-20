//! THE Executor seam: the name is
//! **`Executor`** — the LANEWARDEN vocabulary finalizes here. Since the pooled intake port landed,
//! this module owns the GLOBAL EXECUTOR TOKEN
//! FAMILY for `arb_engine` — `global_executor` (the solve-arm seam) plus
//! the pooled-intake delegate `global_sim_executor` (which hands out
//! `fleet_intake`'s `FleetIntake` port over the pooled sim executor; the
//! registration intake arm is served by the `fleet_intake` facade's
//! `registration_intake`) — and re-exports the shared
//! seam types (mirroring the degenbot-workers placement of shared types —
//! no pyo3 in any signature).
use degenbot_core::op_error;
pub(crate) use degenbot_workers::dispatcher::SubmitReceipt;
use degenbot_workers::dispatcher::{BootError, SubmitError};
use degenbot_workers::lane::LaneCtx;
/// One escalated work item handed through a seat's `LaneCtx` port.
pub(crate) type SubmitWork = Box<dyn FnOnce(&LaneCtx) + Send + 'static>;
/// The ONE executor seam: `boot/bin_count/submit` + the
/// `LaneCtx`/`EscalationPort` contracts. The ONE process posture owner
/// (`degenbot_workers::posture::process_owner`) is fed by the block pump
/// directly, and every fleet host consults the same owner; the seam carries
/// no posture channel.
pub(crate) trait Executor: Send + Sync {
    /// The structural seat count bins bind at.
    fn bin_count(&self) -> usize;
    /// Submit one LPT bin job; the unit body receives the seat's `LaneCtx`.
    /// Admission is posture-invariant: a posture refusal
    /// at the gate aborts LOUDLY (process exit) with the 'bin submission,
    /// posture gate' context; accepted units are never silently dropped
    /// (units ride one of the solved-arm lanes; receipts are tracked per
    /// lane).
    fn submit(&self, bin: usize, work: SubmitWork) -> Result<SubmitReceipt, SubmitError>;
}
/// The solve-arm global token: every SOLVE call site submits
/// through here. It is the solve arm of a token FAMILY — the
/// pooled intake arms submit through `global_sim_executor` here (which
/// delegates to `fleet_intake`'s `FleetIntake` port) and
/// `fleet_intake::registration_intake` (the pyo3-leaf registration arm),
/// not through this fn — the fleet-hosted executors are the only executors
/// (there is no tokio stance and no stance parameter).
/// Construction precondition: the fleet materializes LAZILY on the
/// first call — from the construction-STAMPED boot the first engine
/// construction installed (`with_core_cfg`). There is NO fallback boot: a
/// caller that reaches this seam BEFORE any engine construction is a
/// contract violation and aborts LOUD (`expect` in the materializer).
pub(crate) fn global_executor() -> &'static dyn Executor {
    crate::arb_engine::fleet_solve_executor::global_fleet_solve_executor()
}
/// The pooled SIM intake delegate: hands out `fleet_intake`'s
/// `FleetIntake` port over the fleet sim executor, so the sim dispatch
/// route reads through ONE module (the trait object flows in-crate only).
pub(crate) fn global_sim_executor(
) -> Result<&'static dyn crate::arb_engine::fleet_intake::FleetIntake, BootError> {
    crate::arb_engine::fleet_intake::sim_intake()
}
// ---------------------------------------------------------------------------
// THE solve lane: the one outcome-carrier module both solve
// arms submit through. Folded here from the fleet solve executor's
// provisional lane module — the placement is the named placement.
// WITNESS + CARRIER + LEDGER live together on purpose: the lane knows the
// pids it owes, the carrier carries what the merge consumes, and the ledger
// asserts the one-outcome-per-path-per-cycle law across BOTH arms.
// ---------------------------------------------------------------------------
use crate::arb_engine::inline_sim::SimulatedPathResult;
use crate::arb_engine::solve_cycle::ClampReport;
use crate::arb_engine::{BlockMetadata, SolvePathResult};
use degenbot_workers::dispatcher::{PanicAction, PanicVerdict};
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
/// Why one or more of a unit's paths never delivered an outcome to the
/// result pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneFailure {
    /// The bin closure panicked mid-unit; the seat survives (
    /// decision A) and every undelivered path becomes one of these.
    SeatPanic {
        /// The host-tracked unit id that panicked.
        unit: u64,
        /// The seat (slot) that was executing the unit.
        seat: u64,
        /// The panic payload when it is a string.
        message: Option<String>,
    },
    /// The LANE died mid-flight: the seat thread
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
/// Which seam FINALIZED one lane outcome's executable amounts, outputs, and
/// profit: the handoff the merge reads instead of the retired twin counter.
///
/// The presence of a report means the clamp already committed the figures
/// together, so the merge stores them untouched — a second finalization could
/// only re-apply the margin over committed amounts. Without a report (the
/// stance-off arm) the raw result rides to the merge, which runs that ONE
/// finalization itself.
pub(crate) enum SolveClamp {
    /// The worker clamp produced the final figures; the report owns them.
    Finalized(ClampReport),
    /// No worker clamp ran (the inline stance is off, or this bin has no
    /// pool-ref snapshot): the merge finalizes this raw result exactly once.
    Unfinalized(SolvePathResult),
}

impl SolveClamp {
    /// The result this handoff carries — finalized, or pending finalization.
    pub(crate) fn result(&self) -> &SolvePathResult {
        match self {
            Self::Finalized(report) => &report.result,
            Self::Unfinalized(result) => result,
        }
    }
}

#[cfg(test)]
impl SolveClamp {
    /// The FINALIZED arm over an already-committed result: how the merge/lane
    /// fixtures hand a straggler the values the engine already holds.
    pub(crate) fn finalized_for_test(result: SolvePathResult) -> Self {
        Self::Finalized(ClampReport::for_test(
            result,
            crate::arb_engine::solve_cycle::ClampStatus::Exact,
        ))
    }
}

/// The unified solved payload: the solved arm's `(pid, clamp, payload)` data
/// PLUS the issuing-cycle identity (the exactness-ledger key material + the Q1a
/// oracle) PLUS the per-merge `merge_one_result` arguments the sidecar reads off
/// the item. The detached arm fills `cycle_seq`/`update_stamp`/`solve_span` per
/// enqueue; the in-cycle arm fills them with inert values its drain never reads
/// (`cycle_seq` 0 — its claims use the cycle's own seq).
pub(crate) struct SolveOutcome {
    pub pid: u64,
    /// The result handoff + which seam finalized it (the single-finalization
    /// protocol).
    pub clamp: SolveClamp,
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
    ///. `Span::none()` on the in-cycle arm and in tests.
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
/// One typed terminal record for a DEAD MERGE DRAIN: the merge
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
/// The detached arm's drain-death hook: `Arc`-shared so every
/// bin thread clones it; fired with the typed failure so the hook stays a
/// plain policy function (`drain_death_response`, or a test recorder).
pub(crate) type DrainDeathHook = std::sync::Arc<dyn Fn(&DrainFailure) + Send + Sync>;
/// The drain-death loud-log cadence: EVERY loss is counted (the
/// metric and the posture cause), but the error line is emitted on the
/// first occurrence and then every `DRAIN_DEATH_LOG_EVERY`th, so a dead
/// pipe cannot flood the log while the terminal state stays continuously
/// VISIBLE — a once-only line an operator can miss is not acceptable.
const DRAIN_DEATH_LOG_EVERY: u64 = 256;
static DRAIN_DEATH_LOGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The ONE drain-death response. The merge pipe's only consumer is
/// the sidecar thread, and `spawn_merge_sidecar` spawns exactly one per
/// engine lifetime — a dead merge seat can NEVER recover in-process.
/// CLASSIFICATION: a dead merge seat is NOT a `failure_policy`
/// Fatal/Exit bucket; it is the LANE-DEATH terminal class — a STICKY
/// cordon plus a live (loud) process, exactly like a solve-lane death.
/// `owner` overrides the process posture owner for hermetic tests.
pub(crate) fn drain_death_response(
    failure: &DrainFailure,
    owner: Option<&degenbot_workers::posture::PostureOwner>,
) {
    match failure {
        DrainFailure::SendFailed { .. } | DrainFailure::Stranded { .. } => {
            crate::instruments::record(crate::instruments::CensusEvent::StragglerSendFailed);
        }
        DrainFailure::MergePanic { .. } => {
            crate::instruments::record(crate::instruments::CensusEvent::MergePanic);
        }
    }
    // Feeder-site contract: the wrapper feeds the owner AND wakes
    // the fleet hosts on a real transition — never a raw `observe_cause`.
    crate::arb_engine::fleet_wake::feed_cause(
        owner,
        degenbot_workers::posture::PostureCause::LaneDeath,
    );
    let occurrence = DRAIN_DEATH_LOGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if occurrence == 1 || occurrence.is_multiple_of(DRAIN_DEATH_LOG_EVERY) {
        op_error!(domain = exec, failure = ?failure,
            occurrence,
            "merge drain DEAD — outcome lost and counted, sticky posture cordon set (FF-T4 lane death); the process lives, only a fresh process lifts it"
        );
    }
}
/// Why the lane witness refused a record: the per-path obligation table could
/// not conclude the pid the caller named. Every refusal is LOGGED at the door
/// (lane identity + reason), so a contradicted record is never a silent drop —
/// and never a delivery, because the obligation IS the record's only carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObligationRefusal {
    /// The lane never submitted this pid: the witness owes exactly its bin's
    /// paths, so there is no obligation to conclude.
    UnknownPid { pid: u64 },
    /// The pid already left the lane exactly once — a second terminal record.
    DuplicateTerminal { pid: u64 },
    /// A solved outcome arrived for a path that already answered it (parked or
    /// concluded): one submitted path has one solve.
    DuplicateSolved { pid: u64 },
    /// A sim receipt arrived for a pid the lane already concluded.
    LateReceipt { pid: u64 },
    /// A sim receipt arrived for a submitted pid with nothing parked: no sim
    /// was scheduled for it, or the receipt was already handed back.
    NothingParked { pid: u64 },
}

/// One submitted path's obligation — the witness's per-pid state, and the ONE
/// place a path's presence lives (the owed list, the parked outcome, and the
/// emitted marker are this table, never parallel containers).
// Deliberate: the parked arm carries the merge payload inline — the solve arms
// are throughput-bound per seat and a parked outcome is consumed (removed) the
// moment its receipt lands, so a Box would only add a per-sim heap hop to the
// handoff.
#[expect(
    clippy::large_enum_variant,
    reason = "the parked outcome is unboxed because it is consumed on its receipt"
)]
enum Obligation {
    /// The lane owes this path one terminal record; nothing parked yet.
    Submitted,
    /// A clamp-committed `Solved` outcome is parked on the path until its
    /// scheduled sim's receipt lands, or until a failure patch concludes it.
    Parked(SolveOutcome),
    /// The path left the lane: exactly one terminal record went on the pipe.
    Emitted,
}

/// The lane WITNESS for one bin: one per-path obligation, concluded exactly
/// once — delivered outcomes as they happen, undelivered ones patched as typed
/// `Failed` records after a panic (the seat survives) or a lane death.
pub(crate) struct SolveLane {
    unit: u64,
    seat: u64,
    /// One entry per submitted path (`Submitted` at construction).
    obligations: BTreeMap<u64, Obligation>,
    tx: mpsc::Sender<LaneOutcome>,
    /// the DETACHED arm's in-flight gauge hook, fired on a `Solved` item's
    /// SEND SUCCESS only (never for Suppressed/Failed — those never bump, so
    /// they may never decrement).
    on_solved_send: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// the DETACHED arm's drain-death hook, fired on a terminal `LaneOutcome`
    /// send FAILURE (the merge pipe's Receiver is gone). Strictly additive: the
    /// failed send is NOT re-delivered and the in-flight gauge is NOT bumped —
    /// the ledger is owed deliveries for SUBMITTED paths only, so this surfaces
    /// the DRAIN DEATH instead of fabricating a pipe delivery.
    on_send_failed: Option<DrainDeathHook>,
}

impl SolveLane {
    /// Build a lane for one bin: `unit`/`seat` name the accounting identity the
    /// failure records carry; `pids` are the paths the bin's work owes the pipe,
    /// each seeded as one `Submitted` obligation.
    ///
    /// The hooks are fused into the constructor — the lane's hook state is
    /// immutable after construction, so there is no window in which a driven
    /// lane could lack its hooks.
    pub(crate) fn new(
        unit: u64,
        seat: u64,
        pids: Vec<u64>,
        tx: mpsc::Sender<LaneOutcome>,
        on_solved_send: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
        on_send_failed: Option<DrainDeathHook>,
    ) -> Self {
        let owed = pids.len();
        let mut obligations: BTreeMap<u64, Obligation> = BTreeMap::new();
        for pid in pids {
            obligations.insert(pid, Obligation::Submitted);
        }
        // The bin plan is one row per path, so a repeated pid would collapse two
        // obligations into one and silently under-count the ledger.
        debug_assert_eq!(obligations.len(), owed, "submitted paths must be distinct");
        Self {
            unit,
            seat,
            obligations,
            tx,
            on_solved_send,
            on_send_failed,
        }
    }

    /// Conclude one path with the worker's `Some` arm, for a path no sim rode.
    pub(crate) fn solved(&mut self, item: SolveOutcome) -> Result<(), ObligationRefusal> {
        let pid = item.pid;
        self.open(pid)?;
        self.conclude(pid, LaneOutcome::Solved(item));
        Ok(())
    }

    /// Conclude the worker's `None` arm — still an outcome (counted).
    pub(crate) fn suppressed(&mut self, pid: u64) -> Result<(), ObligationRefusal> {
        self.open(pid)?;
        self.conclude(pid, LaneOutcome::Suppressed { pid });
        Ok(())
    }

    /// Park a clamp-committed outcome on its path: the path stays owed that
    /// SAME record, concluded by its scheduled sim's receipt (or by a failure
    /// patch). A second outcome for one path is refused rather than overwriting
    /// the one already in flight.
    pub(crate) fn park(&mut self, item: SolveOutcome) -> Result<(), ObligationRefusal> {
        let pid = item.pid;
        self.open(pid)?;
        self.obligations.insert(pid, Obligation::Parked(item));
        Ok(())
    }

    /// Deliver one scheduled sim's receipt: it concludes the path's parked
    /// outcome, attaching the payload the sim produced. A receipt for a path
    /// with nothing parked (no sim was scheduled for it, or the receipt was
    /// already handed back) and a receipt arriving after the path's terminal
    /// record are both refusals — a receipt is never silently dropped.
    pub(crate) fn receipt(
        &mut self,
        pid: u64,
        payload: Option<SimulatedPathResult>,
    ) -> Result<(), ObligationRefusal> {
        let Some((_, obligation)) = self.obligations.remove_entry(&pid) else {
            return Err(self.refuse(ObligationRefusal::UnknownPid { pid }));
        };
        let Obligation::Parked(mut item) = obligation else {
            let refusal = match obligation {
                Obligation::Emitted => ObligationRefusal::LateReceipt { pid },
                _ => ObligationRefusal::NothingParked { pid },
            };
            self.obligations.insert(pid, obligation);
            return Err(self.refuse(refusal));
        };
        item.payload = payload;
        self.conclude(pid, LaneOutcome::Solved(item));
        Ok(())
    }

    /// Patch EVERY still-owed path with the failure `make` names: the terminal
    /// receipts for a panic (the seat survives) or a lane death. The patch runs
    /// through the same terminal door as any other record, so patching again
    /// finds its obligations already concluded and patches NOTHING — the
    /// exactly-once law holds by construction. Returns the patched count.
    pub(crate) fn fail_owed(&mut self, make: impl Fn(u64) -> LaneFailure) -> usize {
        let owed = self.owed();
        for &pid in &owed {
            self.conclude(
                pid,
                LaneOutcome::Failed {
                    pid,
                    failure: make(pid),
                },
            );
        }
        owed.len()
    }

    /// The ONE open check: a new record for a path needs an unconcluded,
    /// un-parked obligation.
    fn open(&self, pid: u64) -> Result<(), ObligationRefusal> {
        match self.obligations.get(&pid) {
            Some(Obligation::Submitted) => Ok(()),
            Some(Obligation::Parked(_)) => {
                Err(self.refuse(ObligationRefusal::DuplicateSolved { pid }))
            }
            Some(Obligation::Emitted) => {
                Err(self.refuse(ObligationRefusal::DuplicateTerminal { pid }))
            }
            None => Err(self.refuse(ObligationRefusal::UnknownPid { pid })),
        }
    }

    /// THE terminal door: the pid's obligation becomes `Emitted` and exactly one
    /// record goes on the pipe. A `Solved` record bumps the detached in-flight
    /// gauge at SEND SUCCESS only — a failed send is not a delivery.
    fn conclude(&mut self, pid: u64, record: LaneOutcome) {
        let bumps_gauge = matches!(record, LaneOutcome::Solved(_));
        self.obligations.insert(pid, Obligation::Emitted);
        if self.tx.send(record).is_ok() {
            if bumps_gauge {
                if let Some(gauge) = self.on_solved_send.as_ref() {
                    gauge();
                }
            }
        } else {
            self.note_send_failed(pid);
        }
    }

    /// The ONE refusal door: every contradicted record is logged HERE with the
    /// lane's identity, so no caller can turn a refusal into a silent drop. The
    /// value returns for the callers that assert or react to it.
    fn refuse(&self, refusal: ObligationRefusal) -> ObligationRefusal {
        op_error!(domain = exec, unit = self.unit, seat = self.seat, refusal = ?refusal,
            "lane witness REFUSED a record — one obligation per submitted path (exactness)"
        );
        refusal
    }

    /// surface a terminal send failure that would otherwise be
    /// swallowed. Strictly additive — no pipe delivery is fabricated (the pid is
    /// already `Emitted`, the double-delivery guard) and no gauge is touched; the
    /// hook is the detached arm's drain-death signal.
    fn note_send_failed(&self, pid: u64) {
        if let Some(hook) = self.on_send_failed.as_ref() {
            hook(&DrainFailure::SendFailed {
                pid,
                unit: self.unit,
                seat: self.seat,
            });
        }
    }

    /// The pids this lane still owes the pipe (submitted or parked, not yet
    /// concluded) — the set a panic/lane-death patch pays.
    pub(crate) fn owed(&self) -> Vec<u64> {
        self.obligations
            .iter()
            .filter(|(_, obligation)| !matches!(obligation, Obligation::Emitted))
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// How many paths are parked on their sim receipt (the walk's read: a bin
    /// body must conclude every parked outcome before it returns).
    pub(crate) fn parked(&self) -> usize {
        self.obligations
            .values()
            .filter(|obligation| matches!(obligation, Obligation::Parked(_)))
            .count()
    }
}

/// The lane-death response: a lane that died
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
    let unit = lane.unit;
    let seat = lane.seat;
    let patched = lane.fail_owed(|_| LaneFailure::LaneDeath { unit, seat });
    // Feeder-site contract: the wrapper feeds the owner AND wakes
    // the fleet hosts on a real transition — never a raw `observe_cause`.
    crate::arb_engine::fleet_wake::feed_cause(
        owner,
        degenbot_workers::posture::PostureCause::LaneDeath,
    );
    op_error!(domain = exec, unit,
        seat,
        patched,
        "lane died mid-flight — terminal failure records patched, posture cordoned (sticky), the process lives (FF-T4)"
    );
    patched
}
/// Drive one bin body under the lane witness: a panic is caught (the seat
/// backstop also survives), the `PanicVerdict` is consulted, and every
/// still-unemitted path is patched onto the pipe as exactly one typed
/// `Failed(LaneFailure::SeatPanic)` record — so BOTH arms satisfy
/// "one outcome per submitted path" even through a panic. A bin body
/// that RETURNS with still-owed paths is a lane death (the seat
/// abandoned its bin mid-flight): the lane-death response fires —
/// terminal receipts + the cordoned posture + a live process — instead of
/// a silent sender-drop undercount that trips the merge fuse.
pub(crate) fn run_solve_lane(
    lane: &mut SolveLane,
    verdict: &dyn PanicVerdict,
    work: impl FnOnce(&mut SolveLane),
) {
    let outcome = AssertUnwindSafe(|| work(lane));
    let outcome = std::panic::catch_unwind(outcome);
    let Err(payload) = outcome else {
        // A body that RETURNS with obligations still owed abandoned its bin
        // mid-flight: that is a lane death, not a clean cycle.
        if !lane.owed().is_empty() {
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
            let (unit, seat) = (lane.unit, lane.seat);
            let patched = lane.fail_owed(|_| LaneFailure::SeatPanic {
                unit,
                seat,
                message: message.clone(),
            });
            op_error!(domain = exec, unit,
                seat,
                message = ?message,
                patched,
                "bin job panicked — seat survives with typed failure records (decision A)"
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
/// THE outcome ledger: ONE implementation asserting one
/// typed outcome per path per cycle exactly once, across BOTH solve arms
/// (the in-cycle drain and the detached merge sidecar). Keyed
/// `(solve_seq, pid)`; the prune-age constant is the sidecar ledger age.
pub(crate) mod outcome_ledger {
    /// How many recent solve cycles the ledger spans before pruning
    /// (carried unchanged from the former sidecar ledger age constant;
    /// the boot
    /// in-flight cap bounds meaningful straggler age at ~8, so 64 keeps a
    /// duplicate permanently longer than any straggler can live — uniform
    /// across both arms).
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
    use degenbot_pools::solve::SolvePathResult;
    use degenbot_workers::dispatcher::SeatSurvivesPolicy;
    use degenbot_workers::posture::{FleetPosture, PostureOwner, PosturePolicy, ThrottleSample};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    fn solved(pid: u64) -> SolveOutcome {
        SolveOutcome {
            pid,
            clamp: SolveClamp::finalized_for_test(SolvePathResult::default()),
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
    /// An outcome send against a DROPPED merge
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
        let mut lane = SolveLane::new(
            7,
            3,
            vec![11],
            tx,
            Some(Arc::new(move || {
                gauge_hook.fetch_add(1, Ordering::Relaxed);
            })),
            Some(Arc::new(move |failure| {
                seen_hook.lock().expect("hook mutex").push(failure.clone());
            })),
        );
        lane.solved(solved(11))
            .expect("the send-failure hook fires on the lane's send, not on a refusal");
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
    /// The drain-death response trips the STICKY
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

    /// A lane over `pids` with an inert pipe: the obligation tests read the
    /// records back off the receiver and need no gauge/drain-death hooks.
    fn lane_with(pids: Vec<u64>) -> (SolveLane, std::sync::mpsc::Receiver<LaneOutcome>) {
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        (SolveLane::new(5, 2, pids, tx, None, None), rx)
    }

    /// A minimal sim payload: the receipt tests only need identity to prove the
    /// payload lands on the record that concludes the path.
    fn payload(path_id: u64) -> SimulatedPathResult {
        SimulatedPathResult {
            path_id,
            gross_profit: alloy::primitives::U256::ZERO,
            net_profit: alloy::primitives::U256::ZERO,
            gas_used: 0,
            priority_fee: 0,
            base_fee_next: 0,
            execute_calldata: Vec::new(),
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: 0,
            failure: None,
        }
    }

    /// ONE per-path obligation: a second terminal record for a path that already
    /// left the lane is REFUSED — never a second pipe delivery — whatever class
    /// the first record carried.
    #[test]
    fn a_duplicate_terminal_record_is_refused_instead_of_redelivered() {
        let (mut lane, rx) = lane_with(vec![11]);
        lane.solved(solved(11))
            .expect("the first Solved record concludes the path");
        assert_eq!(
            lane.solved(solved(11)),
            Err(ObligationRefusal::DuplicateTerminal { pid: 11 }),
            "a second Solved record for one path is refused"
        );
        assert_eq!(
            lane.suppressed(11),
            Err(ObligationRefusal::DuplicateTerminal { pid: 11 }),
            "a Suppressed record cannot follow a path's terminal record"
        );
        assert!(
            lane.owed().is_empty(),
            "the path is concluded exactly once, so nothing is still owed"
        );
        drop(lane);
        assert_eq!(
            rx.into_iter().count(),
            1,
            "exactly one record reached the pipe"
        );
    }

    /// The witness owes exactly the bin plan's paths: a record for a path this
    /// lane never submitted is refused instead of being delivered as an outcome
    /// no submitter is waiting for.
    #[test]
    fn a_record_for_an_unsubmitted_path_is_refused() {
        let (mut lane, rx) = lane_with(vec![11]);
        assert_eq!(
            lane.suppressed(99),
            Err(ObligationRefusal::UnknownPid { pid: 99 }),
            "a Suppressed record for an unsubmitted path is refused"
        );
        assert_eq!(
            lane.solved(solved(99)),
            Err(ObligationRefusal::UnknownPid { pid: 99 }),
            "a Solved record for an unsubmitted path is refused"
        );
        assert_eq!(
            lane.receipt(99, None),
            Err(ObligationRefusal::UnknownPid { pid: 99 }),
            "a sim receipt for an unsubmitted path is refused"
        );
        assert_eq!(
            lane.owed(),
            vec![11],
            "the lane's obligation table is untouched by refused records"
        );
        drop(lane);
        assert_eq!(rx.into_iter().count(), 0, "nothing reached the pipe");
    }

    /// The solved outcome parks on its path until the scheduled sim's receipt
    /// lands; the receipt concludes it ONCE, carrying the payload — and a
    /// second, late receipt after the terminal record is refused rather than
    /// silently dropped.
    #[test]
    fn a_late_sim_receipt_after_emission_is_refused() {
        let (mut lane, rx) = lane_with(vec![13]);
        lane.park(solved(13))
            .expect("a clamp-committed outcome parks on its path");
        assert_eq!(lane.parked(), 1, "the parked outcome is in flight");
        assert_eq!(
            lane.owed(),
            vec![13],
            "a parked path is still owed its terminal record"
        );
        lane.receipt(13, Some(payload(13)))
            .expect("the parked outcome's receipt concludes it");
        assert_eq!(lane.parked(), 0, "the receipt concluded the parked outcome");
        assert_eq!(
            lane.receipt(13, Some(payload(13))),
            Err(ObligationRefusal::LateReceipt { pid: 13 }),
            "a receipt arriving after the terminal record is refused"
        );
        drop(lane);
        let records: Vec<LaneOutcome> = rx.into_iter().collect();
        assert_eq!(records.len(), 1, "one record per submitted path");
        let item = records
            .into_iter()
            .find_map(|record| match record {
                LaneOutcome::Solved(item) => Some(item),
                _ => None,
            })
            .expect("the receipt delivers the Solved outcome");
        assert_eq!(
            item.payload.as_ref().map(|p| p.path_id),
            Some(13),
            "the receipt's payload rides the record that concludes the path"
        );
    }

    /// A receipt for a path with no parked outcome — the walk scheduled no sim
    /// for it, or the receipt was already handed back — is refused, and a park
    /// cannot follow a path's terminal record.
    #[test]
    fn a_sim_receipt_with_nothing_parked_is_refused() {
        let (mut lane, rx) = lane_with(vec![17]);
        assert_eq!(
            lane.receipt(17, None),
            Err(ObligationRefusal::NothingParked { pid: 17 }),
            "a submitted path with nothing parked cannot take a receipt"
        );
        lane.solved(solved(17))
            .expect("the path concludes on its own solve");
        assert_eq!(
            lane.park(solved(17)),
            Err(ObligationRefusal::DuplicateTerminal { pid: 17 }),
            "a parked outcome cannot follow the path's terminal record"
        );
        drop(lane);
        assert_eq!(rx.into_iter().count(), 1);
    }

    /// Lane death pays every still-owed path exactly once — a Submitted path AND
    /// a path parked on its sim receipt both get their terminal record — and a
    /// second death response patches nothing (every path is already concluded),
    /// so the ledger cannot double-count.
    #[test]
    fn a_lane_death_patches_every_owed_path_exactly_once() {
        let owner = hermetic_owner();
        let (mut lane, rx) = lane_with(vec![20, 21, 22]);
        lane.suppressed(20)
            .expect("the first path concludes on its own solve");
        lane.park(solved(21))
            .expect("the second path parks on its sim receipt");
        assert_eq!(
            lane_death_response(&mut lane, Some(owner)),
            2,
            "the parked outcome and the untouched path are both still owed"
        );
        assert_eq!(
            lane_death_response(&mut lane, Some(owner)),
            0,
            "a second death response patches nothing: every path is concluded"
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        assert_eq!(
            lane.receipt(21, Some(payload(21))),
            Err(ObligationRefusal::LateReceipt { pid: 21 }),
            "the parked outcome's receipt arrives after its terminal record"
        );
        drop(lane);
        let records: Vec<LaneOutcome> = rx.into_iter().collect();
        assert_eq!(records.len(), 3, "one terminal record per submitted path");
        let failed: Vec<u64> = records
            .iter()
            .filter_map(|record| match record {
                LaneOutcome::Failed { pid, .. } => Some(*pid),
                _ => None,
            })
            .collect();
        assert_eq!(
            failed,
            vec![21, 22],
            "the parked path and the untouched path land exactly once each"
        );
    }

    /// A seat panic concludes the parked path exactly once (one typed
    /// `SeatPanic` record), and the sim receipt that lands afterwards is
    /// refused: no path is both failed and delivered.
    #[test]
    #[expect(clippy::panic)]
    fn a_seat_panic_concludes_a_parked_path_exactly_once() {
        let (mut lane, rx) = lane_with(vec![30, 31]);
        run_solve_lane(&mut lane, &SeatSurvivesPolicy, |lane| {
            lane.suppressed(30)
                .expect("the first path concludes on its own solve");
            lane.park(solved(31))
                .expect("the second path parks on its sim receipt");
            panic!("bin panics with one path parked (obligation harness)");
        });
        assert!(
            lane.owed().is_empty(),
            "the panic patch covers every path (the parked one included)"
        );
        assert_eq!(
            lane.receipt(31, None),
            Err(ObligationRefusal::LateReceipt { pid: 31 }),
            "the parked outcome's receipt arrives after the panic patch"
        );
        drop(lane);
        let records: Vec<LaneOutcome> = rx.into_iter().collect();
        assert_eq!(records.len(), 2, "one terminal record per submitted path");
        let failed: Vec<(u64, LaneFailure)> = records
            .into_iter()
            .filter_map(|record| match record {
                LaneOutcome::Failed { pid, failure } => Some((pid, failure)),
                _ => None,
            })
            .collect();
        assert_eq!(
            failed,
            vec![(
                31,
                LaneFailure::SeatPanic {
                    unit: 5,
                    seat: 2,
                    message: Some(
                        "bin panics with one path parked (obligation harness)".to_owned()
                    ),
                }
            )],
            "the parked path lands as exactly one typed failure record"
        );
    }
}
