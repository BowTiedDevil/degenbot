//! The ONE host-message/waiting shape (ADR-079 D1): the unified
//! [`HostPump`] — `apply_host_msg` + the backlog-draining grant pump +
//! the recv loop — with the `HostMsg` vocabulary, the progress states,
//! and the seat-model / discipline seams ([`SeatSink`], [`HostDiscipline`]).

use super::admission::{
    admission, AdmissionInputs, GrantContract, NoProgressGuard, NoProgressStep,
};
use degenbot_core::op_error;
use degenbot_workers::dispatcher::{EnqueueError, FleetHost, Grant, GrantKind, Unit};
use degenbot_workers::role::WorkerRole;
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;
/// Host-bound message — ONE shape for all three fleet hosts: a
/// submitted unit, or a seat reporting its unit done. Completion applies
/// the slot's own T-row ([`FleetHost::complete`] keys off the slot FSM
/// state: T5 pooled → idle, T3 Solver → warm re-pin), so the message
/// needs no per-role arm.
pub(crate) enum HostMsg {
    Enqueue(Unit),
    SeatDone {
        seat: u64,
    },
    /// An untrusted posture-transition hint (T3): it carries NO value — the
    /// pump that follows re-reads the live owner, so a spurious, lost, or
    /// coalesced edge is benign. It can only cause an earlier wake.
    PostureEdge,
}
/// The host intake's progress vocabulary. `Idle`/`Backed` are live in this
/// refactor (the backlog drain loop keys on `Backed`); `Faulted`/`Closed`
/// are introduced by the tasks whose transitions make them reachable (the
/// no-progress loud-fail and the lane-death fault), so no unconstructed
/// variant ships here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressState {
    /// The backlog is empty — nothing held.
    Idle,
    /// The backlog is non-empty — held units await admission.
    Backed,
    /// The sticky lane-death latch is held (FF-T4): no admit can ever arrive,
    /// so held work is resolved terminally instead of parked. Keyed on the
    /// TYPED latch only — a long recoverable cordon stays `Backed`. Sticky:
    /// once the latch is set there is no edge out (a fresh process only).
    Faulted,
}
/// The seat-model split of the unified host-message triple. The
/// triple (admission, backlog drain, grant loop) is ONE shape; routing a
/// GRANTED unit to its seat stays per host kind — the pooled `WorkQueue`
/// condvar pair vs the solve host's per-seat keyed mailboxes (warm arenas,
/// typed `LaneCtx`). Folding the seat models themselves is the
/// misfit; this trait is the seam that keeps them apart.
pub(crate) trait SeatSink {
    /// Route one T2-started grant to its seat. `host` is handed back
    /// because the solve sink mints the grant slot's warm arena
    /// (`ensure_arena`) at the same seam the pre-fold loop did.
    fn deliver(&self, host: &mut FleetHost, grant: Grant, unit: Unit);
}
/// The per-host loud-abort + wording seam of the unified triple: the
/// model-specific CONTEXT strings stay owned by their host module (the
/// pooled pair's ride the descriptor; solve's are its own), while the
/// stranded-pipe EMISSION is the ONE shared template (ADR-079 D3 —
/// `seat_host::abort_unrecoverable`). The unified code calls the SEMANTIC
/// operations only.
pub(crate) trait HostDiscipline {
    /// Unrecoverable host failure at a SHARED context ("backlog drain",
    /// "grant start (T2)") — never returns.
    fn fail(&self, context: &str, err: &str) -> !;
    /// The apply-time enqueue refusal — ONE pattern on both models:
    /// `"{noun} enqueue"` (the keyed host rides its mirrored noun).
    fn enqueue_refused(&self, err: &str) -> !;
    /// The `SeatDone` completion refusal — ONE wording on both models
    /// (ADR-079 D3): `"seat completion (T5)"` (the applied T-row itself is
    /// slot-FSM data, keyed off the slot state — see [`HostMsg::SeatDone`]).
    fn completion_refused(&self, err: &str) -> !;
    /// The row-#6 grant-kind contract denial (context `"dispatch grant"`).
    fn foreign_grant(&self, kind: GrantKind) -> !;
}
/// THE one host-message/waiting shape: `apply_host_msg` + the
/// backlog-draining grant pump + the recv loop, folded ONCE behind all
/// three fleet hosts. The pooled pair (sim + registration) and the solve
/// host parameterize it — everything the diverged pair actually
/// differed on is a field:
///
/// - [`SeatSink`] — the seat model split: the pooled
///   `WorkQueue` vs the solve host's per-seat keyed mailboxes.
/// - `mirror: Option<&AtomicUsize>` — the typed-submit receipt's
///   advisory stamp (solve only; `None` on the fire-and-forget pooled
///   port). It stores the BOUNDED role-queue length at the last stamp
///   (spill or `SeatDone`) — NOT the backlog depth: the receipt bit means
///   "the role queue was >= cap at the last stamp", an advisory lagging
///   flag exactly as `SubmitReceipt`'s doc says. (the
///   "backlog mirror" comment no longer overstates; the mechanism is kept.)
/// - [`GrantContract`] — the row-#6 grant-kind contract, now explicit on
///   all three hosts (solve GAINED it: pre-fold a foreign grant reached
///   the solve pump only to die by the `seats.get(slot)` indexing
///   accident — or, under Nominal, to be silently SEATED on a Solver
///   seat; the pooled side aborted loudly).
/// - [`HostDiscipline`] — the per-host abort funnel into the ONE
///   emission (ADR-079 D3; the contexts stay model-owned).
///
/// # Posture consult (behavior-EXACT for solve — spike-proven)
///
/// The consult runs UNCONDITIONALLY in the apply pre-check and the drain
/// guard. For the solve host this is provably a no-op:
/// `FleetHost::posture_admits_role` is `posture.admits_lease(role.
/// cordon_class())`, and `admits_lease` blocks ONLY the
/// `(FleetPosture::Cordoned, CordonClass::Deferrable)` pair
/// (`degenbot_workers::posture`), while `WorkerRole::Solver.cordon_class()`
/// is `CordonClass::Never` (`degenbot_workers::role`) — so the consult is
/// constant-true for Solver and the guard reduces to the cap-only check
/// the solve loop had pre-fold. The same proof covers the `try_enqueue`
/// hand-back arm: `EnqueueError::PostureHeld` fires only for Deferrable
/// units, never a Solver unit. Do NOT re-add a Solver cordon gate by hand
/// the consult's unconditional presence here IS the point (one shape,
/// one consult, class-derived).
///
/// # Wake discipline (the ratified contract — ADR-044)
///
/// The pump runs on a message AND on the `BackstopTick`: `recv_timeout` is
/// armed iff the backlog is non-empty (T2), so a guard change that arrives
/// with no message — a posture lift from the block-pump thread — still
/// re-runs the pump within the backstop. The posture owner does not signal
/// host channels directly (layering); instead every owner-mutating feeder
/// emits a `PostureEdge` (an untrusted, seq-stamped hint) through the
/// bot-side waker (`arb_engine::fleet_wake`), which promptly wakes parked
/// hosts. Both paths re-read the LIVE owner — neither carries a posture
/// value. A held backlog is therefore drained by the next tick at the
/// latest, never parked forever and never dropped (§10).
///
/// The retired client-fairness assumption (that a steady stream of
/// submissions would itself re-wake a held backlog) is FALSIFIED by the
/// driver's bounded submission window (`REG_INTAKE_WINDOW`) and must not be
/// reintroduced.
pub(crate) struct HostPump<'a> {
    /// The FSM — exclusively owned by this host thread.
    pub(crate) host: &'a mut FleetHost,
    /// The unbounded §10 backlog (FIFO; drains FIRST on every pass).
    pub(crate) backlog: &'a mut VecDeque<Unit>,
    /// The hosted role — the descriptor's pooled role or the Solver pin
    /// role; drives the queue len/cap keys and the posture consult.
    pub(crate) role: WorkerRole,
    /// The row-#6 grant-kind contract (loud on all three hosts).
    pub(crate) grants: GrantContract,
    /// The seat model.
    pub(crate) sink: &'a dyn SeatSink,
    /// The typed-receipt advisory mirror (solve only; `None` = pooled).
    pub(crate) mirror: Option<&'a AtomicUsize>,
    /// The per-host abort wording owner.
    pub(crate) discipline: &'a dyn HostDiscipline,
    /// The recv-timeout backstop (T2): armed iff the backlog is non-empty,
    /// so a guard change arriving with no message (a posture lift from the
    /// block-pump thread) still re-runs the pump. Read once per host loop
    /// from the typed config.
    pub(crate) backstop: Duration,
    /// The T4 no-progress guard (K consecutive admitted-but-no-progress
    /// passes trip the loud fail). Borrowed so it persists across every
    /// pump pass of one host thread.
    pub(crate) no_progress: &'a mut NoProgressGuard,
    /// The S2 fault watch: set on entering Faulted so every
    /// parked intake receipt resolves terminally. `None` on hosts whose
    /// receipts are not pyo3-owned (sim/solve): `progress()` requires a
    /// watch, so they NEVER enter Faulted — held work there follows the
    /// pre-existing lane-death flows (S2 scope cut: registration intake).
    pub(crate) fault: Option<&'a crate::arb_engine::fleet_intake::IntakeFaultWatch>,
    /// Test seam (T2): counts timeout-driven backstop pumps so the tick rate
    /// can be asserted bounded. Absent from production builds.
    #[cfg(test)]
    pub(crate) ticks: Option<&'a AtomicU64>,
}
impl HostPump<'_> {
    /// Build the complete admission input tuple from the live host — the
    /// ONLY host read that feeds [`admission`].
    fn admission_inputs(&self) -> AdmissionInputs {
        AdmissionInputs {
            queue_len: self.host.queue_len(self.role),
            queue_cap: self.host.queue_cap(self.role),
            posture_admits: self.host.posture_admits_role(self.role),
        }
    }
    /// The intake progress state (backlog emptiness today; the fault and
    /// closed arms land with the transitions that make them reachable).
    fn progress(&self) -> ProgressState {
        // Faulted is a PURE function of the sticky typed latch:
        // no separate flag, so double lane-death delivery is idempotent by
        // construction and no elapsed-time heuristic can reach it. Gated on
        // a fault watch: only receipt-owning hosts Fault — a SHARED process
        // posture owner's sticky latch must never fault a host with no
        // receipts (a cross-test contamination class).
        if self.fault.is_some() && self.host.lane_death_held() {
            ProgressState::Faulted
        } else if self.backlog.is_empty() {
            ProgressState::Idle
        } else {
            ProgressState::Backed
        }
    }
    /// Publish the live backlog depth to the `degenbot_fleet_intake_backlog`
    /// gauge. Called at every pump exit and after a fault drain,
    /// so a stalled held backlog is observable.
    fn publish_backlog(&self) {
        crate::instruments::record(crate::instruments::CensusEvent::IntakeBacklog {
            role: self.role.label(),
            depth: self.backlog.len() as u64,
        });
    }
    /// The Faulted arm: drain the backlog AND the role queue and
    /// report the held count to the S2 fault watch. Granted in-flight units
    /// are untouched (they complete naturally). A held unit runs ZERO times
    /// here — resolution != execution, so at-most-once holds. Idempotent:
    /// the watch is first-wins, the drains are clears, and a repeat pass is
    /// a no-op once both are empty.
    fn resolve_fault(&mut self) {
        let held = self.backlog.len() + self.host.queue_len(self.role);
        self.backlog.clear();
        let _ = self.host.drain_role_queue(self.role);
        self.publish_backlog();
        if let Some(fault) = self.fault {
            if fault.snapshot().is_none() {
                fault.set(crate::arb_engine::fleet_intake::IntakeFault {
                    cause: "lane-death",
                    held,
                });
                op_error!(domain = solver, role = ?self.role,
                    held,
                    "lane-death latch held — Faulted: {held} queued/backlogged unit(s) resolved terminally (never executed), in-flight units complete naturally"
                );
            }
        }
    }
    /// The ONE dispatch loop (design doc §4): recv → apply → pump. Exits
    /// when the submission channel closes (all executor handles dropped —
    /// process teardown).
    ///
    /// # Lint note
    /// `rx` is taken by value under an explicit lint expectation: the
    /// Receiver's ownership moves into the spawned host thread — a borrow
    /// cannot cross the thread boundary.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the host Receiver's ownership moves into the spawned host thread — a borrow cannot cross the thread boundary"
    )]
    pub(crate) fn run(mut self, rx: mpsc::Receiver<HostMsg>) {
        loop {
            // Arm the backstop IFF a backlog exists (T2): a held backlog is
            // the only state whose guards can change with NO message (the
            // posture owner is fed by the block-pump thread). A Backed host
            // therefore wakes on the timer and re-runs the pump; an empty
            // host keeps the blocking recv (no busy-spin when parked empty).
            let msg = if self.progress() == ProgressState::Backed {
                match rx.recv_timeout(self.backstop) {
                    Ok(msg) => Some(msg),
                    // The mandatory liveness input: re-evaluate every mutable
                    // guard, then drain whatever the lift unblocked.
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        #[cfg(test)]
                        if let Some(ticks) = self.ticks {
                            ticks.fetch_add(1, Ordering::Relaxed);
                        }
                        None
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => break,
                }
            };
            if let Some(msg) = msg {
                self.apply_host_msg(msg);
            }
            self.pump();
        }
    }
    /// Apply one submission or completion (both arrive on the single host
    /// channel — completions can never starve behind a blocking recv).
    pub(crate) fn apply_host_msg(&mut self, msg: HostMsg) {
        match msg {
            HostMsg::Enqueue(unit) => {
                // Pre-check capacity INSTEAD of failing enqueue: the host
                // thread owns every queue mutation, so the check is exact.
                // Units that do not fit spill to the backlog (unbounded,
                // like the legacy pipelines) and drain FIRST on the next
                // pump — never dropped (§10 ledger).
                //
                // The pre-check ALSO consults the ONE shared posture owner
                //, unconditionally. For Solver this is
                // provably a no-op (the struct doc's proof:
                // `admits_lease` blocks only `(Cordoned, Deferrable)`;
                // `Solver` is `CordonClass::Never`) — the consult's
                // presence is the unified shape, not a new gate.
                if !admission(self.admission_inputs()).admits() {
                    self.backlog.push_back(unit);
                    if let Some(mirror) = self.mirror {
                        // The spill stamp: the receipt's advisory reads the
                        // BOUNDED role-queue length at the stamp — not the
                        // backlog depth.
                        mirror.store(self.host.queue_len(self.role), Ordering::Relaxed);
                    }
                } else if let Err((err, unit)) = self.host.try_enqueue(unit) {
                    if matches!(err, EnqueueError::PostureHeld(_)) {
                        // The shared owner is fed from the throttle-poller
                        // thread (the block pump), NOT this host thread — a
                        // cordon can onset between the check and the
                        // enqueue. Hold, never drop, never abort (§10): the
                        // gate handed the unit BACK (`try_enqueue`), so it
                        // waits in the backlog and re-queues on the next
                        // pump. (For Solver this arm is dead — the proof
                        // above; its presence is the unified hand-back
                        // seam, which the pre-fold solve loop LACKED: the
                        // same input aborted the process.)
                        self.backlog.push_back(unit);
                    } else {
                        // v1-active, non-merge units of this host's role
                        // cannot hit RoleNotActive / MergeNeverQueued, and
                        // QueueFull cannot fire behind the exact same-thread
                        // capacity check. Any such error is a broken
                        // invariant, not a drop.
                        self.discipline.enqueue_refused(&err.to_string());
                    }
                }
            }
            HostMsg::SeatDone { seat } => {
                if let Err(err) = self.host.complete(seat) {
                    // Completion applies the slot's own T-row (T5 pooled →
                    // idle / T3 Solver → warm re-pin) — keyed off the slot
                    // FSM state, so ONE shape serves all three hosts.
                    self.discipline.completion_refused(&err.to_string());
                }
                if let Some(mirror) = self.mirror {
                    mirror.store(self.host.queue_len(self.role), Ordering::Relaxed);
                }
            }
            // T3: an untrusted posture hint. No value is read — the pump
            // that follows re-reads the live owner.
            HostMsg::PostureEdge => {}
        }
    }
    /// The one precedence grant loop pass (design doc §4): backlog first,
    /// then dispatch grants onto the seats. Grants apply T2 (start) at
    /// grant time — the seat-model delivery IS the claim — and completion
    /// arrives via [`HostMsg::SeatDone`].
    pub(crate) fn pump(&mut self) {
        // Faulted FIRST: the sticky lane-death latch means no
        // admit can ever arrive, so held work is resolved terminally (and
        // this returns before the T4 guard, which must never accrue here).
        if self.progress() == ProgressState::Faulted {
            self.resolve_fault();
            return;
        }
        // T4: snapshot the pre-pass shape for the no-progress guard. A pass
        // is "admitted" iff there is held work AND the admission predicate
        // would admit it; it "progressed" iff the backlog shrank or a grant
        // was delivered. A legitimate WaitCap/WaitPosture hold is therefore
        // never counted (admitted is false), and the counter resets ONLY on
        // real progress or an un-admitted pass — never on a bare wake.
        let backlog_before = self.backlog.len();
        let admitted = !self.backlog.is_empty() && admission(self.admission_inputs()).admits();
        // Backlog drains FIRST (FIFO across the loud-overflow seam). A
        // backed backlog that cannot enqueue (the cordon hold of Deferrable
        // intake, or a full per-role queue) parks here until the next host
        // message — the awaiting callers already submitted, and retrying on
        // every wake matches the legacy worker semantics (work waits, never
        // drops). Every unit in this host's backlog carries the host's role
        // by construction (the submit seams stamp it), so the role check is
        // the host's own role.
        while self.progress() == ProgressState::Backed {
            // The per-pop guard re-reads the WHOLE admission tuple live
            // (no mirrors): the cap and the posture consult (Solver:
            // provably constant-true — the struct doc's proof; the term
            // exists so nobody re-adds a Solver cordon gate by hand).
            if !admission(self.admission_inputs()).admits() {
                break;
            }
            let Some(unit) = self.backlog.pop_front() else {
                break;
            };
            if let Err((err, unit)) = self.host.try_enqueue(unit) {
                if matches!(err, EnqueueError::PostureHeld(_)) {
                    // The shared owner is fed from the throttle-poller
                    // thread — a cordon can onset between the check and the
                    // enqueue. The gate handed the unit BACK
                    // (`try_enqueue`): the head goes back (FIFO order
                    // preserved) and the drain parks until the next host
                    // message — hold, never drop, never abort (§10).
                    self.backlog.push_front(unit);
                    break;
                }
                self.discipline.fail("backlog drain", &err.to_string());
            }
        }
        let mut granted = 0usize;
        loop {
            let grants = self.host.dispatch();
            if grants.is_empty() {
                break;
            }
            for (grant, unit) in grants {
                // Row #6, loud on ALL THREE hosts: this host
                // only enqueues its own role's units, so every grant must
                // be one of its contracted kinds. Anything else is a broken
                // host contract, not a drop. (The solve host GAINED this
                // check — pre-fold a foreign grant reached it only to die
                // by the `seats.get` indexing accident, or — under
                // Nominal — to be silently SEATED on a Solver seat.)
                if !self.grants.admits(grant.kind) {
                    self.discipline.foreign_grant(grant.kind);
                }
                if let Err(err) = self.host.start(grant.slot, &unit) {
                    self.discipline.fail("grant start (T2)", &err.to_string());
                }
                self.sink.deliver(self.host, grant, unit);
                granted += 1;
            }
        }
        let progressed = granted > 0 || self.backlog.len() < backlog_before;
        self.publish_backlog();
        if let NoProgressStep::Tipping { consecutive } = self.no_progress.step(admitted, progressed)
        {
            self.discipline.fail(
                "no progress (T4)",
                &format!(
                    "admission admitted a non-empty backlog but {consecutive} consecutive pump passes made no progress (K = {}): the dispatch/admission invariant is broken and held work would park forever",
                    self.no_progress.limit()
                ),
            );
        }
    }
}

#[cfg(test)]
use super::admission::{intake_no_progress_ticks, Admission};

#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic)]
mod tests {
    // -----------------------------------------------------------------------
    // The cross-host property suite. The unified HostPump is driven
    // DIRECTLY in both parameterizations (the dispatcher::harness precedent —
    // the core is exercised by a deterministic script, not by production
    // callers), with the seat-model artifacts (seat counts, keyed-pin chain,
    // mailbox routing, arena ids) normalized away by a recording sink.
    // -----------------------------------------------------------------------
    use super::{
        admission, intake_no_progress_ticks, Admission, AdmissionInputs, GrantContract,
        HostDiscipline, HostMsg, HostPump, NoProgressGuard, NoProgressStep, ProgressState,
        SeatSink,
    };
    use crate::arb_engine::fleet_intake::IntakeFault;
    use crate::arb_engine::fleet_solve_executor::SOLVE_BIN_KEY_BASE;
    use degenbot_workers::budget::BudgetOverrides;
    use degenbot_workers::dispatcher::{FleetBoot, FleetHost, Grant, GrantKind, Unit};
    use degenbot_workers::posture::{
        FleetPosture, PostureCause, PostureOwner, PosturePolicy, ThrottleSample,
    };
    use degenbot_workers::role::WorkerRole;
    use proptest::prelude::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    /// A FRESH hermetic posture owner (leaked to 'static): every test boot
    /// gets its own owner, never the process global.
    /// Per-file hermeticity stays per-file (the kill list): this
    /// suite owns its helper and does not couple the other test binaries.
    fn hermetic_owner() -> &'static PostureOwner {
        std::boxed::Box::leak(std::boxed::Box::new(PostureOwner::new(
            PosturePolicy::doc_defaults(),
        )))
    }
    /// Force the shared owner Cordoned (the event-burst trigger the
    /// dispatcher fixtures use).
    fn force_cordoned(owner: &'static PostureOwner) {
        owner.observe_throttle(
            0,
            ThrottleSample {
                events: 3,
                throttled_usec: 0,
                elapsed_usec: 100_000,
            },
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
    }
    /// Feed the full clean hysteresis (10 s of virtual clean ticks since
    /// the dirty sample at now = 0) — the lift.
    fn lift_cordon(owner: &'static PostureOwner) {
        let mut now = 1_000;
        loop {
            owner.observe_throttle(
                now,
                ThrottleSample {
                    events: 0,
                    throttled_usec: 0,
                    elapsed_usec: 1_000,
                },
            );
            if owner.current() == FleetPosture::Nominal {
                break;
            }
            now += 1_000;
            assert!(now <= 60_000, "the cordon never lifted");
        }
    }
    /// The hermetic fleet boot shared by the driven host and the real-thread
    /// backstop test (T2).
    fn hermetic_fleet_boot(owner: &'static PostureOwner) -> FleetBoot {
        FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 8.0,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: Some(owner),
        }
    }
    /// The recorded outcome of a driven pass — seat-model artifacts
    /// (`SeatJob` shapes, mailbox routing, arena ids, seat-thread identity)
    /// normalized away to what the cross-host property asserts: WHICH unit
    /// got seated WHERE, and which contract denial fired.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        /// A granted unit reached a seat (unit id, granted slot).
        Seated { unit: u64, slot: u64 },
        /// The row-#6 grant-kind denial fired (recorded, not aborted).
        ForeignGrant(GrantKind),
        /// T4: the no-progress loud fail fired (recorded, then panicked so
        /// the drive can catch it — production aborts the process).
        NoProgressAbort,
    }
    /// One shared recorder behind the sink + the discipline.
    struct Recorder {
        outcomes: parking_lot::Mutex<Vec<Outcome>>,
    }
    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                outcomes: parking_lot::Mutex::new(Vec::new()),
            })
        }
        fn record(&self, outcome: Outcome) {
            self.outcomes.lock().push(outcome);
        }
        fn snapshot(&self) -> Vec<Outcome> {
            self.outcomes.lock().clone()
        }
    }
    /// Recording sink: grants land here instead of real seats (the seat
    /// models themselves are pinned by the executors' own suites — the
    /// property normalizes them away).
    struct RecordingSink {
        recorder: Arc<Recorder>,
    }
    impl SeatSink for RecordingSink {
        fn deliver(&self, _host: &mut FleetHost, grant: Grant, _unit: Unit) {
            self.recorder.record(Outcome::Seated {
                unit: grant.unit,
                slot: grant.slot,
            });
        }
    }
    /// Recording discipline: the row-#6 denial becomes DATA (the
    /// `VerdictRecorder` idiom — never a real process abort under test);
    /// any other failure is an unexpected shape and panics loudly.
    struct RecordingDiscipline {
        recorder: Arc<Recorder>,
    }
    impl HostDiscipline for RecordingDiscipline {
        fn fail(&self, context: &str, err: &str) -> ! {
            self.recorder.record(Outcome::NoProgressAbort);
            panic!("host failure recorded at {context}: {err}")
        }
        fn enqueue_refused(&self, err: &str) -> ! {
            panic!("unexpected enqueue refusal: {err}")
        }
        fn completion_refused(&self, err: &str) -> ! {
            panic!("unexpected completion refusal: {err}")
        }
        fn foreign_grant(&self, kind: GrantKind) -> ! {
            self.recorder.record(Outcome::ForeignGrant(kind));
            panic!("Row-#6 denial recorded: {kind:?}");
        }
    }
    /// The two host kinds the property drives (the parameterization IS the
    /// fold map).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HostKind {
        /// The pooled pair's registration arm: `PoolStateUpdater` (Deferrable
        /// the cordon hold is reachable), one grant kind, no
        /// receipt mirror (the fire-and-forget port).
        Pooled,
        /// The solve host: `Solver` (`CordonClass::Never`), the pin-pair
        /// grant contract, the receipt mirror present.
        Solve,
    }
    /// One driven host: a REAL hermetic `FleetHost` behind the unified
    /// `HostPump`, driven by direct `HostMsg` sequences — no seat threads,
    /// fully deterministic; the sink records the grants.
    struct DrivenHost {
        kind: HostKind,
        host: FleetHost,
        backlog: VecDeque<Unit>,
        sink: RecordingSink,
        discipline: RecordingDiscipline,
        recorder: Arc<Recorder>,
        mirror: Option<Arc<AtomicUsize>>,
        cursor: usize,
        in_flight: VecDeque<u64>,
        unit_seq: u64,
        /// T4: the persistent no-progress guard (a real drive reuses ONE
        /// guard across passes, mirroring production).
        guard: NoProgressGuard,
        /// T6: the S2 fault watch this driven host reports to (leaked to
        /// 'static — hermetic per test).
        fault: &'static crate::arb_engine::fleet_intake::IntakeFaultWatch,
        /// T2: the recv-timeout backstop used by a real-thread drive.
        backstop: Duration,
    }
    impl DrivenHost {
        fn boot(kind: HostKind) -> Self {
            Self::boot_with(kind, hermetic_owner())
        }
        fn boot_with(kind: HostKind, owner: &'static PostureOwner) -> Self {
            let host = FleetHost::boot(hermetic_fleet_boot(owner)).expect("hermetic fleet boot");
            let recorder = Recorder::new();
            let mirror = match kind {
                HostKind::Pooled => None,
                HostKind::Solve => Some(Arc::new(AtomicUsize::new(0))),
            };
            Self {
                kind,
                host,
                backlog: VecDeque::new(),
                sink: RecordingSink {
                    recorder: Arc::clone(&recorder),
                },
                discipline: RecordingDiscipline {
                    recorder: Arc::clone(&recorder),
                },
                recorder,
                mirror,
                cursor: 0,
                in_flight: VecDeque::new(),
                unit_seq: 0,
                guard: NoProgressGuard::new(intake_no_progress_ticks()),
                fault: std::boxed::Box::leak(std::boxed::Box::new(
                    crate::arb_engine::fleet_intake::IntakeFaultWatch::new(),
                )),
                backstop: Duration::from_millis(50),
            }
        }
        fn role(&self) -> WorkerRole {
            match self.kind {
                HostKind::Pooled => WorkerRole::PoolStateUpdater,
                HostKind::Solve => WorkerRole::Solver,
            }
        }
        /// The role's next unit (pooled: no key; solve: the ONE bin key —
        /// the single-key chain keeps the drive deterministic: a hot key
        /// waits for its own seat's T6 continuation, so grants are
        /// strictly ordered).
        fn next_unit(&mut self) -> Unit {
            self.next_unit_keyed(Some(SOLVE_BIN_KEY_BASE))
        }
        /// The role's next unit with an EXPLICIT solve pin key (the Q3.4
        /// drive pins TWO seats so the role queue dips below cap while the
        /// backlog is still non-empty — the honest-mirror window).
        fn next_unit_keyed(&mut self, solve_key: Option<u64>) -> Unit {
            self.unit_seq += 1;
            let key = match self.kind {
                HostKind::Pooled => None,
                HostKind::Solve => solve_key,
            };
            Unit::new(self.unit_seq, self.role(), key, false, Box::new(|_ctx| {}))
        }
        fn pump_handle(&mut self) -> HostPump<'_> {
            // Read the copy fields BEFORE the mutable borrows (disjoint
            // field borrows read fine, but the role() method call would
            // re-borrow all of self).
            let role = self.role();
            let grants = match self.kind {
                HostKind::Pooled => GrantContract::Single(GrantKind::PoolStateUpdate),
                HostKind::Solve => GrantContract::SolverPins,
            };
            HostPump {
                host: &mut self.host,
                backlog: &mut self.backlog,
                role,
                grants,
                sink: &self.sink,
                mirror: self.mirror.as_deref(),
                discipline: &self.discipline,
                backstop: self.backstop,
                no_progress: &mut self.guard,
                fault: Some(self.fault),
                #[cfg(test)]
                ticks: None,
            }
        }
        /// Apply one host message + run one grant pass (the `run()` loop's
        /// per-message shape, driven directly).
        fn apply_and_pump(&mut self, msg: HostMsg) {
            let mut pump = self.pump_handle();
            pump.apply_host_msg(msg);
            pump.pump();
        }
        /// Like `Self::apply_and_pump`, but the row-#6 denial (a recorded,
        /// deliberate panic under the recording discipline) is caught —
        /// the discipline makes the loud abort DATA for the property;
        /// production uses the real process abort.
        fn apply_and_pump_catching_denial(&mut self, msg: HostMsg) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.apply_and_pump(msg);
            }));
            let _ = result;
        }
        /// Absorb newly recorded grants into the in-flight queue (the
        /// slots awaiting their `SeatDone` message).
        fn absorb_new_grants(&mut self) {
            let snap = self.recorder.snapshot();
            while self.cursor < snap.len() {
                if let Outcome::Seated { slot, .. } = snap[self.cursor] {
                    self.in_flight.push_back(slot);
                }
                self.cursor += 1;
            }
        }
        /// Complete the oldest in-flight grant (one `SeatDone` message +
        /// pass — what a real seat thread does).
        fn complete_oldest_in_flight(&mut self) {
            self.absorb_new_grants();
            let slot = self
                .in_flight
                .pop_front()
                .expect("an in-flight grant to complete");
            self.apply_and_pump(HostMsg::SeatDone { seat: slot });
        }
        /// `SeatDone` choreography: complete outstanding grants, one message
        /// + pass at a time, until want units are seated.
        fn drain_until_seated(&mut self, want: usize) {
            loop {
                self.absorb_new_grants();
                if self.seated_units().len() >= want {
                    break;
                }
                let slot = self
                    .in_flight
                    .pop_front()
                    .expect("an in-flight grant to complete — the drive must have granted first");
                self.apply_and_pump(HostMsg::SeatDone { seat: slot });
            }
            self.absorb_new_grants();
        }
        /// Complete EVERY outstanding grant (the drive runs the host to
        /// quiescence: no unit left running, the final stamp reads the
        /// empty role queue).
        fn run_to_quiescence(&mut self) {
            loop {
                self.absorb_new_grants();
                if self.in_flight.is_empty() {
                    break;
                }
                self.complete_oldest_in_flight();
            }
            self.absorb_new_grants();
        }
        fn seated_units(&self) -> Vec<u64> {
            self.recorder
                .snapshot()
                .iter()
                .filter_map(|outcome| match outcome {
                    Outcome::Seated { unit, .. } => Some(*unit),
                    Outcome::ForeignGrant(_) | Outcome::NoProgressAbort => None,
                })
                .collect()
        }
    }
    /// THE cross-host property: drive IDENTICAL unit sequences
    /// into both host kinds and assert IDENTICAL backlog ORDER outcomes —
    /// every unit seated exactly once, in SUBMISSION order (the §10
    /// spill-to-backlog + drain-first FIFO), with the backlog empty at the
    /// end. Seat-model artifacts (when the spill begins, seat counts,
    /// keyed-pin chaining) are normalized away; the queue-cap geometry is
    /// the one legitimate host-kind difference the catalog preserves.
    #[test]
    fn cross_host_identical_unit_sequences_yield_identical_backlog_order() {
        for kind in [HostKind::Pooled, HostKind::Solve] {
            let mut driven = DrivenHost::boot(kind);
            // cap + 2 with seats left busy: the queue fills to the role
            // cap, the last units spill to the unbounded backlog, and the
            // drain-first pump must reorder NOTHING (the same N = cap + 7
            // units into both kinds — the SAME sequence shape, both spill).
            let n = driven.host.queue_cap(driven.role()) + 7;
            for _ in 0..n {
                let unit = driven.next_unit();
                driven.apply_and_pump(HostMsg::Enqueue(unit));
            }
            driven.drain_until_seated(n);
            driven.run_to_quiescence();
            let want: Vec<u64> = (1..=n as u64).collect();
            assert_eq!(
                driven.seated_units(),
                want,
                "{kind:?}: the backlog ORDER outcome must be the submission order                  (the spill never reorders, the drain-first pump is FIFO)"
            );
            assert!(
                driven.backlog.is_empty(),
                "{kind:?}: the backlog must drain empty (never dropped, §10)"
            );
            if let Some(mirror) = &driven.mirror {
                assert_eq!(
                    mirror.load(Ordering::Relaxed),
                    0,
                    "{kind:?}: at rest the role queue is empty and the stamp says so"
                );
            }
        }
    }
    /// Q3.3 (the `push_front` head-preservation pin): hold three pooled
    /// units under a forced cordon (forced BEFORE any unit is in flight —
    /// a cordon onset SHEDS running Deferrable units to Draining (T7),
    /// whose completion rides T8, not the T5 `SeatDone` arm), lift, and
    /// wake the host with ONE message — the held three drain in
    /// SUBMISSION order (the backlog head order survived the hold; the
    /// drain re-queues FIFO).
    #[test]
    fn held_backlog_requeues_fifo_after_the_cordon_lifts() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        // Force the shared owner Cordoned first — Deferrable intake HOLDS.
        force_cordoned(owner);
        for _ in 0..3 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        assert!(
            driven.seated_units().is_empty(),
            "the cordon HOLDS Deferrable intake — nothing granted"
        );
        assert_eq!(
            driven.backlog.len(),
            3,
            "the held units wait in the unbounded backlog, never dropped"
        );
        // Lift, then wake the parked backlog with ONE submission message.
        lift_cordon(owner);
        let unit = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(unit));
        driven.run_to_quiescence();
        let seated = driven.seated_units();
        assert_eq!(seated.len(), 4, "all four units seated — nothing dropped");
        let held: Vec<u64> = seated.iter().filter(|u| **u <= 3).copied().collect();
        assert_eq!(
            held,
            vec![1, 2, 3],
            "the held units drain in submission order after the lift              (FIFO head order preserved: 1 before 2 before 3)"
        );
        assert!(
            driven.backlog.is_empty(),
            "the backlog drained empty (never dropped, §10)"
        );
    }
    /// THE posture-consult unification, behavior-pinned (catalog row #1
    /// dissolved): the SAME input — one unit submitted under a FORCED
    /// CORDON — into both host kinds. The unified shape consults posture
    /// unconditionally; the outcomes differ ONLY by the role's own cordon
    /// class, never by host-kind code: the Deferrable registration host
    /// HOLDS intake in the backlog, while the solve host ADMITS
    /// and grants under the cordon — the consult is provably
    /// constant-true for `CordonClass::Never`, so the unconditional consult
    /// is behavior-EXACT for solve).
    #[test]
    fn cross_host_cordon_holds_the_deferrable_host_and_never_the_solver_host() {
        // Pooled (registration): held.
        let owner = hermetic_owner();
        let mut pooled = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        let unit = pooled.next_unit();
        pooled.apply_and_pump(HostMsg::Enqueue(unit));
        assert!(
            pooled.seated_units().is_empty(),
            "Deferrable intake is HELD under cordon"
        );
        assert_eq!(
            pooled.backlog.len(),
            1,
            "held = the unbounded backlog, never a drop"
        );
        // Solve: admitted + granted under the SAME cordon.
        let owner = hermetic_owner();
        let mut solve = DrivenHost::boot_with(HostKind::Solve, owner);
        force_cordoned(owner);
        let unit = solve.next_unit();
        solve.apply_and_pump(HostMsg::Enqueue(unit));
        assert_eq!(
            solve.seated_units(),
            vec![1],
            "Solver intake is posture-INVARIANT (CordonClass::Never): the              unified consult admits and grants under the same cordon"
        );
        assert!(
            solve.backlog.is_empty(),
            "the consult never holds a Solver unit"
        );
    }
    /// THE rows-#2 + #6 dissolution, made observable cross-host: a
    /// foreign-role unit reaching each host's pump under a forced cordon.
    /// Pre-fold, the solve host ABORTED THE PROCESS on this exact input
    /// (the red: no `try_enqueue` hand-back arm — the pooled host
    /// held the identical input) and, under Nominal, silently SEATED a
    /// foreign grant via the seats.get indexing accident. The unified
    /// shape: BOTH hosts hold the unit in the backlog during the cordon
    /// (never dropped, §10) and BOTH deny the foreign grant at dispatch
    /// (recorded here, aborted in prod) — the unit is NEVER seated.
    #[test]
    fn cross_host_foreign_unit_is_held_never_dropped_and_denied_at_grant_on_both_hosts() {
        // Solve host + a Deferrable registration unit, cordoned.
        let owner = hermetic_owner();
        let mut solve = DrivenHost::boot_with(HostKind::Solve, owner);
        force_cordoned(owner);
        let foreign = Unit::new(
            9_001,
            WorkerRole::PoolStateUpdater,
            None,
            false,
            Box::new(|_ctx| {}),
        );
        solve.apply_and_pump(HostMsg::Enqueue(foreign));
        assert!(
            !solve.backlog.is_empty(),
            "solve: the foreign Deferrable unit is HELD by the unified              try_enqueue hand-back (pre-fold: process abort — the row-#2 red)"
        );
        assert!(
            solve.recorder.snapshot().is_empty(),
            "nothing seated while held"
        );
        // Lift, then wake the drain with a legal Solver unit: the backlog
        // head (the foreign unit) re-queues into the PoolStateUpdater
        // queue, dispatch grants it — and the unified SolverPins contract
        // DENIES it loudly (recorded), before it could reach any seat.
        lift_cordon(owner);
        let unit = solve.next_unit();
        solve.apply_and_pump_catching_denial(HostMsg::Enqueue(unit));
        let outcomes = solve.recorder.snapshot();
        assert!(
            matches!(
                outcomes.last(),
                Some(Outcome::ForeignGrant(GrantKind::PoolStateUpdate))
            ),
            "solve: the foreign grant is denied loudly at dispatch (row #6): {outcomes:?}"
        );
        assert!(
            !outcomes
                .iter()
                .any(|o| matches!(o, Outcome::Seated { unit: 9_001, .. })),
            "the foreign unit is NEVER seated — no silent misroute, no seat-map accident"
        );
        // The pooled mirror: a foreign SOLVER unit, cordoned — held by the
        // host-role consult, denied at grant by the Single(PoolStateUpdate)
        // contract. IDENTICAL visible outcome: held, never dropped, never
        // seated.
        let owner = hermetic_owner();
        let mut pooled = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        let foreign = Unit::new(
            9_002,
            WorkerRole::Solver,
            // A bin key: Solver leases are PINNED-key (T1's pinnable-key
            // constraints) — a keyless Solver unit could not be granted,
            // and the drive must reach the grant to pin the row-#6 denial.
            Some(SOLVE_BIN_KEY_BASE),
            false,
            Box::new(|_ctx| {}),
        );
        pooled.apply_and_pump(HostMsg::Enqueue(foreign));
        assert!(
            !pooled.backlog.is_empty(),
            "pooled: the foreign unit is HELD under the cordon (the host-role consult)"
        );
        lift_cordon(owner);
        let unit = pooled.next_unit();
        pooled.apply_and_pump_catching_denial(HostMsg::Enqueue(unit));
        let outcomes = pooled.recorder.snapshot();
        assert!(
            matches!(
                outcomes.last(),
                Some(Outcome::ForeignGrant(GrantKind::NewPinClaim))
            ),
            "pooled: the foreign grant is denied loudly at dispatch (row #6): {outcomes:?}"
        );
        assert!(
            !outcomes
                .iter()
                .any(|o| matches!(o, Outcome::Seated { unit: 9_002, .. })),
            "the foreign unit is NEVER seated on the pooled host either"
        );
    }
    /// Q3.4 (the honest mirror): the receipt bit tracks the
    /// BOUNDED role-queue stamp — NOT the backlog depth. After the spill
    /// the mirror equals the cap (bit TRUE: the queue WAS at cap); after
    /// the next `SeatDone` stamp the mirror reads the DRAINED queue (bit
    /// FALSE) while the backlog is still NON-EMPTY — the advisory-lag
    /// semantics `SubmitReceipt` documents, pinned so the corrected comment
    /// cannot rot back into "the backlog mirror".
    #[test]
    fn receipt_bit_tracks_the_role_queue_stamp_not_backlog_depth() {
        let mut solve = DrivenHost::boot(HostKind::Solve);
        let cap = solve.host.queue_cap(WorkerRole::Solver);
        let key_a = Some(SOLVE_BIN_KEY_BASE);
        let key_b = Some(SOLVE_BIN_KEY_BASE + 1);
        // Two bin keys: the first two units pin two seats, so the role
        // queue DIPS BELOW cap while the backlog still holds units — the
        // honest-mirror window (a single-key chain would keep refilling
        // the queue to cap on every drain pass).
        let unit = solve.next_unit_keyed(key_a);
        solve.apply_and_pump(HostMsg::Enqueue(unit));
        let unit = solve.next_unit_keyed(key_b);
        solve.apply_and_pump(HostMsg::Enqueue(unit));
        // Fill the queue to the cap, then spill two.
        let mut flip = false;
        let n = cap + 4;
        for _ in 2..n {
            let unit = solve.next_unit_keyed(if flip { key_b } else { key_a });
            flip = !flip;
            solve.apply_and_pump(HostMsg::Enqueue(unit));
        }
        let mirror = Arc::clone(
            solve
                .mirror
                .as_ref()
                .expect("the solve host carries the receipt mirror"),
        );
        let spilled = mirror.load(Ordering::Relaxed);
        assert_eq!(
            spilled, cap,
            "the spill stamps the BOUNDED role-queue length at the stamp (= cap),              not the backlog size"
        );
        // Complete the two in-flight pins: the first SeatDone stamps the
        // FULL queue; the drain-first pump then grants from the queue
        // faster than the 2-deep backlog can refill it — the SECOND
        // SeatDone stamps a SUB-CAP queue while the backlog still holds
        // units: the bit goes FALSE with a non-empty backlog — it does
        // NOT track backlog depth.
        solve.complete_oldest_in_flight();
        solve.complete_oldest_in_flight();
        let after = mirror.load(Ordering::Relaxed);
        assert!(
            after < cap,
            "the stamp follows the role queue ({after} < cap), not the backlog"
        );
        assert!(
            !solve.backlog.is_empty(),
            "the backlog still holds units while the bit reads FALSE — the stamp              is the role-queue length, an advisory lagging flag"
        );
        // Full drain to quiescence: the final SeatDone stamp reads the
        // empty queue.
        solve.drain_until_seated(n);
        solve.run_to_quiescence();
        assert_eq!(
            mirror.load(Ordering::Relaxed),
            0,
            "at rest the role queue is empty and the stamp says so"
        );
    }
    /// T1: the admission predicate's total truth table — every reachable
    /// (occupancy × cap × posture) combination maps to exactly one
    /// [`Admission`] value. The predicate reads the tuple alone, so the
    /// tuple is constructible here with no live host.
    #[test]
    fn admission_predicate_truth_table() {
        let cases = [
            (0usize, 4usize, true, Admission::Admit),
            (3, 4, true, Admission::Admit),
            (4, 4, true, Admission::WaitCap),
            (5, 4, true, Admission::WaitCap),
            (0, 4, false, Admission::WaitPosture),
            (3, 4, false, Admission::WaitPosture),
            (4, 4, false, Admission::WaitBoth),
            (0, 0, true, Admission::WaitCap),
            (0, 0, false, Admission::WaitBoth),
        ];
        for (queue_len, queue_cap, posture_admits, want) in cases {
            let inputs = AdmissionInputs {
                queue_len,
                queue_cap,
                posture_admits,
            };
            assert_eq!(
                admission(inputs),
                want,
                "admission(len={queue_len}, cap={queue_cap}, admits={posture_admits})"
            );
            // Totality + purity: identical inputs, identical output.
            assert_eq!(admission(inputs), admission(inputs));
        }
    }
    /// T1: `admits()` is the `Admit` arm and nothing else — the single gate.
    #[test]
    fn admission_admits_only_on_the_admit_arm() {
        assert!(Admission::Admit.admits());
        for denied in [
            Admission::WaitCap,
            Admission::WaitPosture,
            Admission::WaitBoth,
        ] {
            assert!(!denied.admits(), "{denied:?} must not admit");
        }
    }
    /// T1: the progress vocabulary is `Backed` iff the backlog is
    /// non-empty; the drain loop keys on it.
    #[test]
    fn progress_state_is_backed_iff_the_backlog_is_non_empty() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        assert_eq!(
            driven.pump_handle().progress(),
            ProgressState::Idle,
            "an empty backlog is Idle"
        );
        let unit = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(unit));
        assert_eq!(
            driven.pump_handle().progress(),
            ProgressState::Backed,
            "a held unit is Backed"
        );
        lift_cordon(owner);
        let unit = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(unit));
        driven.run_to_quiescence();
        assert_eq!(
            driven.pump_handle().progress(),
            ProgressState::Idle,
            "a drained backlog is Idle"
        );
    }
    /// T1: the predicate is the ONLY gate on the backlog → role-queue move.
    /// `WaitPosture` holds with room available; `WaitCap` holds when the
    /// bounded queue is full; `Admit` seats.
    #[test]
    fn admission_is_the_only_gate() {
        // WaitPosture: cordoned, room available.
        let owner = hermetic_owner();
        let mut pooled = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        let unit = pooled.next_unit();
        pooled.apply_and_pump(HostMsg::Enqueue(unit));
        assert!(pooled.seated_units().is_empty());
        assert_eq!(pooled.backlog.len(), 1, "WaitPosture holds in the backlog");
        // Admit: lift — the held unit plus the next drain.
        lift_cordon(owner);
        let unit = pooled.next_unit();
        pooled.apply_and_pump(HostMsg::Enqueue(unit));
        pooled.run_to_quiescence();
        let mut seated = pooled.seated_units();
        seated.sort_unstable();
        assert_eq!(seated, vec![1, 2], "Admit seats every unit exactly once");
        assert!(pooled.backlog.is_empty());
        // WaitCap: all role-queue seats running, then fill the bounded
        // queue to its cap; the next submission spills to the backlog.
        let owner = hermetic_owner();
        let mut capped = DrivenHost::boot_with(HostKind::Pooled, owner);
        let cap = capped.host.queue_cap(capped.role());
        // Fill until the bounded role queue saturates: with no SeatDone,
        // the free slots take units until none are free, then the queue
        // fills to its cap. (Iteration is bounded well above cap.)
        let mut submitted = 0usize;
        while capped.host.queue_len(capped.role()) < cap {
            let unit = capped.next_unit();
            capped.apply_and_pump(HostMsg::Enqueue(unit));
            submitted += 1;
            assert!(
                submitted <= 4 * cap + 2,
                "the bounded role queue never saturated (cap={cap})"
            );
        }
        assert_eq!(capped.host.queue_len(capped.role()), cap);
        assert!(
            capped.backlog.is_empty(),
            "no spill at exactly the cap — WaitCap begins one past it"
        );
        let unit = capped.next_unit();
        capped.apply_and_pump(HostMsg::Enqueue(unit));
        assert_eq!(
            capped.backlog.len(),
            1,
            "WaitCap spills the overflow to the backlog (never dropped)"
        );
    }
    /// T1 transition coverage: an admitted `Enqueue` in `Idle` leaves the host
    /// `Idle` (the unit is granted to a running slot, the backlog stays
    /// empty), and the matching `SeatDone` is likewise an `Idle` self-loop —
    /// the "self-transitions are covered" requirement for the two states
    /// live in this task.
    #[test]
    fn idle_enqueue_and_seatdone_are_self_transitions() {
        let mut driven = DrivenHost::boot(HostKind::Pooled);
        assert_eq!(driven.pump_handle().progress(), ProgressState::Idle);
        let unit = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(unit));
        assert_eq!(
            driven.pump_handle().progress(),
            ProgressState::Idle,
            "an admitted Enqueue grants straight to a slot — Idle self-loop"
        );
        driven.complete_oldest_in_flight();
        assert_eq!(
            driven.pump_handle().progress(),
            ProgressState::Idle,
            "a SeatDone on an empty backlog is an Idle self-loop"
        );
    }
    /// T1 transition coverage, closed (adversarial-review finding): the two
    /// remaining live pairs are Backed self-loops.
    /// (Backed, held Enqueue): a second hold accumulates in FIFO order and
    /// the state stays Backed. (Backed, SeatDone): a completion with the
    /// bounded queue saturated keeps the host Backed; the single pump pass
    /// evaluates the backlog drain BEFORE the grant frees capacity, so the
    /// backlog is untouched in that pass and the freed slot takes the
    /// queued head. The state must NOT fall back to Idle while units
    /// remain held, and no unit may be lost or double-granted. (A later
    /// pass — or T2's `BackstopTick` — drains the backlog into the freed
    /// capacity.)
    #[test]
    fn backed_enqueue_and_seatdone_are_self_transitions() {
        // (Backed, held Enqueue) -> Backed.
        let owner = hermetic_owner();
        let mut held = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        let a = held.next_unit();
        held.apply_and_pump(HostMsg::Enqueue(a));
        assert_eq!(held.pump_handle().progress(), ProgressState::Backed);
        assert_eq!(held.backlog.len(), 1);
        let b = held.next_unit();
        held.apply_and_pump(HostMsg::Enqueue(b));
        assert_eq!(
            held.pump_handle().progress(),
            ProgressState::Backed,
            "a second held Enqueue is a Backed self-loop"
        );
        assert_eq!(held.backlog.len(), 2, "the second hold accumulates");
        let held_ids: Vec<u64> = held.backlog.iter().map(|u| u.id).collect();
        assert_eq!(held_ids, vec![1, 2], "FIFO order preserved across holds");
        // (Backed, SeatDone) -> Backed, with conservation. Saturate the
        // bounded role queue (WaitCap) so no SeatDone can empty it, then
        // spill two units to the backlog.
        let mut capped = DrivenHost::boot(HostKind::Pooled);
        let cap = capped.host.queue_cap(capped.role());
        let mut n = 0usize;
        while capped.host.queue_len(capped.role()) < cap {
            let unit = capped.next_unit();
            capped.apply_and_pump(HostMsg::Enqueue(unit));
            n += 1;
            assert!(n <= 4 * cap + 2, "the role queue never saturated");
        }
        for _ in 0..2 {
            let unit = capped.next_unit();
            capped.apply_and_pump(HostMsg::Enqueue(unit));
        }
        assert_eq!(capped.pump_handle().progress(), ProgressState::Backed);
        assert_eq!(capped.backlog.len(), 2);
        let seated_before = capped.seated_units().len();
        let queued_before = capped.host.queue_len(capped.role());
        capped.complete_oldest_in_flight();
        assert_eq!(
            capped.pump_handle().progress(),
            ProgressState::Backed,
            "a SeatDone under a saturated queue is a Backed self-loop, not Idle"
        );
        assert_eq!(
            capped.backlog.len(),
            2,
            "the backlog is untouched this pass: the drain ran before the grant freed capacity"
        );
        assert_eq!(
            capped.seated_units().len(),
            seated_before + 1,
            "the freed slot granted the queued head exactly once (never dropped)"
        );
        assert_eq!(
            capped.host.queue_len(capped.role()),
            queued_before - 1,
            "the freed queue slot took the queued head, not a backlog unit"
        );
    }
    /// T2 RED-FIRST: the mandatory liveness input. A pooled host with a held
    /// (Backed) backlog under a cordon, driven through the REAL
    /// `HostPump::run` on a thread, must drain within the backstop after the
    /// cordon lifts with ZERO further messages. Pre-fix `run` blocks in
    /// `rx.recv()` forever here (the original 9,955-path deadlock); the
    /// backstop tick re-runs the pump.
    #[test]
    fn backstop_drains_a_held_backlog_when_the_cordon_lifts_with_no_message() {
        let owner = hermetic_owner();
        force_cordoned(owner);
        let recorder = Recorder::new();
        let backstop = Duration::from_millis(50);
        std::thread::scope(|scope| {
            // The channel + backlog live INSIDE the scope body so the
            // sender drops when this body exits (or unwinds). Holding the
            // sender outside would keep the parked child alive and the
            // scope join would block forever, masking any assertion.
            let (tx, rx) = std::sync::mpsc::channel();
            let mut backlog = VecDeque::new();
            let mut host = FleetHost::boot(hermetic_fleet_boot(owner)).expect("hermetic boot");
            let sink = RecordingSink {
                recorder: Arc::clone(&recorder),
            };
            let discipline = RecordingDiscipline {
                recorder: Arc::clone(&recorder),
            };
            scope.spawn(move || {
                HostPump {
                    host: &mut host,
                    backlog: &mut backlog,
                    role: WorkerRole::PoolStateUpdater,
                    grants: GrantContract::Single(GrantKind::PoolStateUpdate),
                    sink: &sink,
                    mirror: None,
                    discipline: &discipline,
                    backstop,
                    no_progress: &mut NoProgressGuard::new(intake_no_progress_ticks()),
                    fault: None,
                    ticks: None,
                }
                .run(rx);
            });
            // Seed one unit under the cordon: it is HELD in the backlog.
            tx.send(HostMsg::Enqueue(Unit::new(
                1,
                WorkerRole::PoolStateUpdater,
                None,
                false,
                Box::new(|_ctx| {}),
            )))
            .expect("submit");
            // Let the host process the message and park with a Backed
            // backlog (still cordoned: nothing may seat).
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                !recorder
                    .snapshot()
                    .iter()
                    .any(|o| matches!(o, Outcome::Seated { .. })),
                "the cordon must HOLD the unit (no seat before the lift)"
            );
            // Lift the cordon WITHOUT any further message.
            lift_cordon(owner);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if recorder
                    .snapshot()
                    .iter()
                    .any(|o| matches!(o, Outcome::Seated { unit: 1, .. }))
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the held unit never drained: the parked host did not wake via BackstopTick (RED-FIRST)"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        });
    }
    /// T2: the backstop tick rate is bounded by TIME, not by message volume.
    /// A cordoned backlog stays Backed while a flood of submissions arrives;
    /// the timeout pumps over a window must be ~window/backstop (independent
    /// of the flood size), nothing may seat before the lift, and the lift
    /// must drain every unit exactly once.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end liveness/tick harness is one driven scenario"
    )]
    fn backstop_tick_rate_is_bounded_by_time_not_messages() {
        let owner = hermetic_owner();
        force_cordoned(owner);
        let recorder = Recorder::new();
        let ticks = Arc::new(AtomicU64::new(0));
        let backstop = Duration::from_millis(40);
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut backlog = VecDeque::new();
            let mut host = FleetHost::boot(hermetic_fleet_boot(owner)).expect("hermetic boot");
            let sink = RecordingSink {
                recorder: Arc::clone(&recorder),
            };
            let discipline = RecordingDiscipline {
                recorder: Arc::clone(&recorder),
            };
            let ticks_in = Arc::clone(&ticks);
            scope.spawn(move || {
                HostPump {
                    host: &mut host,
                    backlog: &mut backlog,
                    role: WorkerRole::PoolStateUpdater,
                    grants: GrantContract::Single(GrantKind::PoolStateUpdate),
                    sink: &sink,
                    mirror: None,
                    discipline: &discipline,
                    backstop,
                    no_progress: &mut NoProgressGuard::new(intake_no_progress_ticks()),
                    fault: None,
                    ticks: Some(&ticks_in),
                }
                .run(rx);
            });
            // Flood while cordoned: every submission is a MESSAGE, not a
            // tick, and every unit must be held (none seated).
            let n: u64 = 40;
            for id in 1..=n {
                tx.send(HostMsg::Enqueue(Unit::new(
                    id,
                    WorkerRole::PoolStateUpdater,
                    None,
                    false,
                    Box::new(|_ctx| {}),
                )))
                .expect("submit");
            }
            let window = Duration::from_millis(400);
            let start = std::time::Instant::now();
            std::thread::sleep(window);
            let elapsed = start.elapsed();
            let observed = ticks.load(Ordering::Relaxed);
            let allowed =
                u64::try_from(elapsed.as_millis() / backstop.as_millis()).unwrap_or(u64::MAX) + 6;
            assert!(
                observed >= 1,
                "the backstop must tick while a backlog is held"
            );
            assert!(
                observed <= allowed && observed < n,
                "ticks {observed} must be bounded by elapsed/backstop ({allowed}) and \
                 far below the {n} messages — bounded by time, not message volume"
            );
            assert!(
                !recorder
                    .snapshot()
                    .iter()
                    .any(|o| matches!(o, Outcome::Seated { .. })),
                "all units are held under the cordon (never dropped, never seated)"
            );
            // Lift: the backlog drains — every unit exactly once. The
            // recording sink is not a real seat, so simulate its SeatDone
            // completions to free slot capacity (the ticks also retry).
            lift_cordon(owner);
            let want = usize::try_from(n).unwrap_or(usize::MAX);
            let mut completed: std::collections::HashSet<u64> = std::collections::HashSet::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let grants: Vec<(u64, u64)> = recorder
                    .snapshot()
                    .iter()
                    .filter_map(|o| match o {
                        Outcome::Seated { unit, slot } => Some((*slot, *unit)),
                        Outcome::ForeignGrant(_) | Outcome::NoProgressAbort => None,
                    })
                    .collect();
                let mut seated: Vec<u64> = grants.iter().map(|(_, unit)| *unit).collect();
                seated.sort_unstable();
                seated.dedup();
                for (slot, unit) in &grants {
                    if completed.insert(*unit) {
                        tx.send(HostMsg::SeatDone { seat: *slot })
                            .expect("complete");
                    }
                }
                if seated.len() == want {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the held backlog never drained after the lift ({}/{} unique)",
                    seated.len(),
                    want
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(
                completed.len(),
                want,
                "every held unit was granted and completed exactly once"
            );
        });
    }
    /// T2 transition coverage: the backstop is armed ONLY in Backed. An Idle
    /// host (empty backlog, open channel) must never tick — the
    /// no-busy-spin-parked-empty contract; `(Idle, BackstopTick)` is a
    /// non-transition by construction.
    #[test]
    fn idle_host_does_not_arm_the_backstop() {
        let owner = hermetic_owner();
        let ticks = Arc::new(AtomicU64::new(0));
        let backstop = Duration::from_millis(30);
        std::thread::scope(|scope| {
            // The sender stays alive for the whole window so the host parks
            // in the blocking recv (Idle) rather than observing a close.
            let (_tx, rx) = std::sync::mpsc::channel();
            let mut backlog = VecDeque::new();
            let mut host = FleetHost::boot(hermetic_fleet_boot(owner)).expect("hermetic boot");
            let recorder = Recorder::new();
            let sink = RecordingSink {
                recorder: Arc::clone(&recorder),
            };
            let discipline = RecordingDiscipline {
                recorder: Arc::clone(&recorder),
            };
            let ticks_in = Arc::clone(&ticks);
            scope.spawn(move || {
                HostPump {
                    host: &mut host,
                    backlog: &mut backlog,
                    role: WorkerRole::PoolStateUpdater,
                    grants: GrantContract::Single(GrantKind::PoolStateUpdate),
                    sink: &sink,
                    mirror: None,
                    discipline: &discipline,
                    backstop,
                    no_progress: &mut NoProgressGuard::new(intake_no_progress_ticks()),
                    fault: None,
                    ticks: Some(&ticks_in),
                }
                .run(rx);
            });
            std::thread::sleep(Duration::from_millis(240));
            assert_eq!(
                ticks.load(Ordering::Relaxed),
                0,
                "an Idle host must not arm the backstop (no busy-spin parked empty)"
            );
        });
    }
    /// T3: an untrusted `PostureEdge` hint wakes a parked backlog far sooner
    /// than a deliberately huge backstop — the hint can only cause an
    /// EARLIER wake, and the pump re-reads the live owner.
    #[test]
    fn posture_edge_wakes_a_parked_backlog_earlier_than_the_backstop() {
        let owner = hermetic_owner();
        force_cordoned(owner);
        let recorder = Recorder::new();
        let backstop = Duration::from_secs(5);
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut backlog = VecDeque::new();
            let mut host = FleetHost::boot(hermetic_fleet_boot(owner)).expect("hermetic boot");
            let sink = RecordingSink {
                recorder: Arc::clone(&recorder),
            };
            let discipline = RecordingDiscipline {
                recorder: Arc::clone(&recorder),
            };
            scope.spawn(move || {
                HostPump {
                    host: &mut host,
                    backlog: &mut backlog,
                    role: WorkerRole::PoolStateUpdater,
                    grants: GrantContract::Single(GrantKind::PoolStateUpdate),
                    sink: &sink,
                    mirror: None,
                    discipline: &discipline,
                    backstop,
                    no_progress: &mut NoProgressGuard::new(intake_no_progress_ticks()),
                    fault: None,
                    ticks: None,
                }
                .run(rx);
            });
            tx.send(HostMsg::Enqueue(Unit::new(
                1,
                WorkerRole::PoolStateUpdater,
                None,
                false,
                Box::new(|_ctx| {}),
            )))
            .expect("submit");
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                !recorder
                    .snapshot()
                    .iter()
                    .any(|o| matches!(o, Outcome::Seated { .. })),
                "held under the cordon"
            );
            lift_cordon(owner);
            // The only wake in this test: an untrusted hint.
            tx.send(HostMsg::PostureEdge).expect("hint");
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            loop {
                if recorder
                    .snapshot()
                    .iter()
                    .any(|o| matches!(o, Outcome::Seated { unit: 1, .. }))
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the PostureEdge hint did not wake the parked backlog (backstop was 5s)"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        });
    }
    /// T3: a `PostureEdge` on an Idle host changes nothing (no backlog, no
    /// seat, still Idle) — the hint is inert without held work.
    #[test]
    fn posture_edge_is_inert_without_a_backlog() {
        let mut driven = DrivenHost::boot(HostKind::Pooled);
        driven.apply_and_pump(HostMsg::PostureEdge);
        assert!(driven.seated_units().is_empty());
        assert!(driven.backlog.is_empty());
        assert_eq!(driven.pump_handle().progress(), ProgressState::Idle);
    }
    /// T3: hint spam never duplicates or drops held units. Several hints
    /// while held leave the backlog untouched; after the lift each unit is
    /// granted exactly once.
    #[test]
    fn posture_edge_never_duplicates_or_drops_held_units() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        for _ in 0..2 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        for _ in 0..5 {
            driven.apply_and_pump(HostMsg::PostureEdge);
        }
        assert_eq!(driven.backlog.len(), 2, "hints are inert while held");
        assert!(driven.seated_units().is_empty());
        lift_cordon(owner);
        for _ in 0..5 {
            driven.apply_and_pump(HostMsg::PostureEdge);
        }
        driven.run_to_quiescence();
        let mut seated = driven.seated_units();
        seated.sort_unstable();
        assert_eq!(seated, vec![1, 2], "each unit granted exactly once");
        assert!(driven.backlog.is_empty());
    }
    /// T3: the hint is untrusted and never a posture value — a `PostureEdge`
    /// on the solve host is a no-op (posture-invariant admission).
    #[test]
    fn posture_edge_does_not_change_the_solve_host() {
        let owner = hermetic_owner();
        let mut solve = DrivenHost::boot_with(HostKind::Solve, owner);
        force_cordoned(owner);
        let unit = solve.next_unit();
        solve.apply_and_pump(HostMsg::Enqueue(unit));
        // Solve admission is posture-invariant: the unit is seated even
        // with the cordon up, and a PostureEdge neither duplicates nor
        // revokes it (the arm reads no value at all).
        assert_eq!(solve.seated_units(), vec![1]);
        solve.apply_and_pump(HostMsg::PostureEdge);
        assert_eq!(solve.seated_units(), vec![1]);
        assert!(solve.backlog.is_empty());
    }
    /// T4 (pure): the guard tips on exactly the K-th consecutive admitted
    /// no-progress pass, and stays tipped afterwards.
    #[test]
    fn no_progress_guard_tips_only_after_k_consecutive_admitted_passes() {
        let mut guard = NoProgressGuard::new(3);
        assert_eq!(
            guard.step(true, false),
            NoProgressStep::Ticking { consecutive: 1 }
        );
        assert_eq!(
            guard.step(true, false),
            NoProgressStep::Ticking { consecutive: 2 }
        );
        assert_eq!(
            guard.step(true, false),
            NoProgressStep::Tipping { consecutive: 3 }
        );
        assert!(matches!(
            guard.step(true, false),
            NoProgressStep::Tipping { .. }
        ));
    }
    /// T4 (pure): real progress resets, and a legitimately UN-admitted pass
    /// (WaitCap/WaitPosture) never accrues — no matter how many passes run.
    #[test]
    fn no_progress_guard_resets_on_progress_and_never_accrues_unadmitted() {
        let mut guard = NoProgressGuard::new(3);
        guard.step(true, false);
        assert_eq!(guard.step(true, true), NoProgressStep::Progressed);
        assert_eq!(guard.consecutive(), 0, "progress resets");
        for _ in 0..10 {
            assert_eq!(guard.step(false, false), NoProgressStep::Progressed);
        }
        assert_eq!(
            guard.consecutive(),
            0,
            "a legitimate hold never accrues toward K"
        );
        // Progress on an un-admitted pass also resets.
        guard.step(true, false);
        guard.step(false, true);
        assert_eq!(guard.consecutive(), 0);
    }
    /// T4 (pure): K is clamped to >= 1 (a garbage config cannot trip on the
    /// first pass).
    #[test]
    fn no_progress_guard_clamps_k_to_at_least_one() {
        let mut guard = NoProgressGuard::new(0);
        assert_eq!(guard.limit(), 1);
        assert_eq!(
            guard.step(true, false),
            NoProgressStep::Tipping { consecutive: 1 }
        );
    }
    /// T4: the loud fail fires on the K-th admitted-but-progressless pass.
    /// The drive parks a Deferrable pooled unit in a Solver host's backlog:
    /// the Solver admission admits (Never-cordoned) while `try_enqueue`
    /// hands the foreign-role unit BACK (`PostureHeld`) — the broken
    /// dispatch/admission invariant the guard exists for.
    #[test]
    fn host_fails_loudly_after_k_admitted_but_progressless_passes() {
        let owner = hermetic_owner();
        force_cordoned(owner);
        let mut driven = DrivenHost::boot_with(HostKind::Solve, owner);
        driven.guard = NoProgressGuard::new(2);
        driven.backlog.push_back(Unit::new(
            1,
            WorkerRole::PoolStateUpdater,
            None,
            false,
            Box::new(|_ctx| {}),
        ));
        driven.pump_handle().pump();
        assert_eq!(driven.guard.consecutive(), 1, "tick 1 of 2");
        assert!(
            !driven
                .recorder
                .snapshot()
                .iter()
                .any(|o| matches!(o, Outcome::NoProgressAbort)),
            "no abort before K"
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            driven.pump_handle().pump();
        }));
        assert!(result.is_err(), "the K-th pass must fail loudly");
        assert!(
            driven
                .recorder
                .snapshot()
                .iter()
                .any(|o| matches!(o, Outcome::NoProgressAbort)),
            "the test seam records the no-progress abort"
        );
    }
    /// T4: an untrusted `PostureEdge` hint runs the SAME no-progress path —
    /// it can never reset K and mask a livelock.
    #[test]
    fn posture_edge_hints_do_not_reset_the_no_progress_counter() {
        let owner = hermetic_owner();
        force_cordoned(owner);
        let mut driven = DrivenHost::boot_with(HostKind::Solve, owner);
        driven.guard = NoProgressGuard::new(3);
        driven.backlog.push_back(Unit::new(
            1,
            WorkerRole::PoolStateUpdater,
            None,
            false,
            Box::new(|_ctx| {}),
        ));
        driven.pump_handle().pump();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            driven.apply_and_pump(HostMsg::PostureEdge);
            assert_eq!(
                driven.guard.consecutive(),
                2,
                "a PostureEdge must not reset K"
            );
            driven.pump_handle().pump();
        }));
        assert!(result.is_err(), "the K-th pass still fails after hints");
    }
    /// T4: a legitimate `WaitPosture` hold parked across far more than K
    /// passes never aborts and never accrues.
    #[test]
    fn a_legitimate_wait_posture_hold_never_accrues_no_progress_ticks() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        for _ in 0..3 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        assert_eq!(driven.backlog.len(), 3, "held under the cordon");
        for _ in 0..driven.guard.limit() * 3 {
            driven.pump_handle().pump();
            assert_eq!(
                driven.guard.consecutive(),
                0,
                "a WaitPosture hold never accrues toward K"
            );
        }
    }
    /// T4: a legitimate `WaitCap` hold (all seats busy, role queue at cap)
    /// parked across far more than K passes never aborts and never accrues.
    #[test]
    fn a_legitimate_wait_cap_hold_never_accrues_no_progress_ticks() {
        let mut driven = DrivenHost::boot(HostKind::Pooled);
        let cap = driven.host.queue_cap(driven.role());
        // Fill the role queue to the cap while every seat is busy (no
        // SeatDone is ever sent), then push one more: the next unit can
        // only spill, and stays a WaitCap hold.
        for _ in 0..=(cap + cap / 2) {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        assert!(
            !driven.backlog.is_empty(),
            "the overflow past the cap is a WaitCap hold"
        );
        for _ in 0..driven.guard.limit() * 3 {
            driven.pump_handle().pump();
            assert_eq!(
                driven.guard.consecutive(),
                0,
                "a WaitCap hold never accrues toward K"
            );
        }
    }
    /// T4 (unreachability): a HEALTHY host — the production geometry, where
    /// every admitted unit can reach a seat — never accrues a single
    /// no-progress tick, even under a tiny K. Only a broken
    /// dispatch/admission invariant can reach the trip.
    #[test]
    fn a_healthy_host_never_reaches_the_no_progress_trip() {
        let mut driven = DrivenHost::boot(HostKind::Pooled);
        driven.guard = NoProgressGuard::new(3);
        let cap = driven.host.queue_cap(driven.role());
        for _ in 0..(cap * 2) {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
            assert_eq!(
                driven.guard.consecutive(),
                0,
                "a healthy host never accrues toward K"
            );
        }
        driven.run_to_quiescence();
        assert!(driven.backlog.is_empty(), "the healthy host drained");
        assert_eq!(driven.guard.consecutive(), 0);
    }
    /// T6: force the STICKY lane-death latch (not a throttle cordon).
    fn force_lane_death(owner: &'static PostureOwner) {
        owner.observe_cause(PostureCause::LaneDeath);
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        assert!(owner.lane_death_held(), "the lane-death latch is sticky");
    }
    /// T6: forced lane death with a non-empty backlog -> Faulted resolves ALL
    /// held units with the typed terminal record; no infinite ticking.
    #[test]
    fn lane_death_faults_held_intake_and_resolves_all() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        // Park three units under a RECOVERABLE EventBurst cordon FIRST, then
        // upgrade to the sticky lane-death latch.
        force_cordoned(owner);
        for _ in 0..3 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        assert_eq!(
            driven.backlog.len(),
            3,
            "parked under the recoverable cordon"
        );
        assert_eq!(
            driven.fault.snapshot(),
            None,
            "no fault while merely cordoned"
        );
        force_lane_death(owner);
        driven.pump_handle().pump();
        assert_eq!(
            driven.pump_handle().progress(),
            super::ProgressState::Faulted
        );
        assert!(driven.backlog.is_empty(), "the held backlog is resolved");
        assert_eq!(
            driven.fault.snapshot(),
            Some(IntakeFault {
                cause: "lane-death",
                held: 3
            }),
            "every held unit is accounted to the typed terminal record"
        );
        assert!(driven.seated_units().is_empty(), "held units never ran");
    }
    /// T6: Faulted is keyed on the TYPED latch, never elapsed time — a long
    /// recoverable EventBurst/Duty cordon must NOT fault.
    #[test]
    fn a_long_recoverable_cordon_never_faults() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        for _ in 0..2 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        for _ in 0..(driven.guard.limit() * 5) {
            driven.pump_handle().pump();
            assert_eq!(
                driven.pump_handle().progress(),
                super::ProgressState::Backed,
                "a recoverable cordon stays Backed"
            );
            assert_eq!(driven.fault.snapshot(), None, "no fault from elapsed time");
        }
        assert_eq!(
            driven.backlog.len(),
            2,
            "held work is never resolved by a recoverable cordon"
        );
    }
    /// T6: entering Faulted for the SAME lane death is idempotent under
    /// double delivery — the first fault record wins and drains are no-ops.
    #[test]
    fn faulted_is_idempotent_under_double_lane_death() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_cordoned(owner);
        for _ in 0..2 {
            let unit = driven.next_unit();
            driven.apply_and_pump(HostMsg::Enqueue(unit));
        }
        force_lane_death(owner);
        driven.pump_handle().pump();
        let first = driven
            .fault
            .snapshot()
            .expect("the first fault is recorded");
        assert_eq!(first.held, 2);
        // A second delivery of the SAME lane death is Held, and re-pumping a
        // Faulted host must not overwrite the first record.
        assert_eq!(
            owner.observe_cause(PostureCause::LaneDeath),
            degenbot_workers::posture::PostureChange::Held
        );
        driven.pump_handle().pump();
        assert_eq!(driven.fault.snapshot(), Some(first), "first-wins");
        assert!(driven.backlog.is_empty());
    }
    /// T6: in-flight granted units are NOT cancelled — they complete naturally
    /// after Faulted (and are never double-resolved).
    #[test]
    fn in_flight_units_complete_naturally_after_faulted() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        let unit = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(unit));
        assert_eq!(driven.seated_units(), vec![1], "granted while Nominal");
        force_lane_death(owner);
        driven.pump_handle().pump();
        assert_eq!(
            driven.fault.snapshot(),
            Some(IntakeFault {
                cause: "lane-death",
                held: 0
            }),
            "the granted unit is not held work"
        );
        driven.complete_oldest_in_flight();
        assert_eq!(driven.seated_units(), vec![1], "completed exactly once");
    }
    /// T6: a unit enqueued AFTER Faulted is resolved terminally, never run.
    #[test]
    fn enqueue_after_faulted_is_resolved_not_run() {
        let owner = hermetic_owner();
        let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
        force_lane_death(owner);
        driven.pump_handle().pump();
        let first = driven
            .fault
            .snapshot()
            .expect("the fault is recorded even with zero held");
        let late = driven.next_unit();
        driven.apply_and_pump(HostMsg::Enqueue(late));
        assert!(
            driven.backlog.is_empty(),
            "the late unit is resolved, not held"
        );
        assert!(
            driven.seated_units().is_empty(),
            "a Faulted host never runs new work"
        );
        assert_eq!(
            driven.fault.snapshot(),
            Some(first),
            "the record is first-wins"
        );
    }
    // ── T7: mechanized safety, liveness, reachability ────────────
    //
    // Falsification contract (adversarial-review requirement): every property
    // below states HOW it fails, not merely that it passes.
    //   * S (safety ledger) — the FIRST symbol at which the conservation
    //     equation breaks is the falsifying trace; proptest shrinks the
    //     symbol sequence to that prefix and the assert prints it.
    //   * L (liveness) — the falsifying trace is a reachable state whose
    //     backlog the pure predicate ADMITS but ONE BackstopTick pump fails
    //     to shrink; the admission tuple is printed.
    //   * BackstopTick-alone — the falsifying trace is a reachable admitted
    //     backlog still non-empty after a bare pump (no hint messages).
    //   * Reachability — the falsifying trace is a reachable non-Faulted
    //     Backed state with NO enabled transition (the predicate does not
    //     admit and the backstop is not armed).
    /// The property alphabet: abstract host inputs a real drive can deliver.
    /// `Backstop` is the recv-timeout tick; `Cordon`/`Lift` are the
    /// recoverable posture edges; `Edge` is the no-op posture notification.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sym {
        Enqueue,
        SeatDone,
        Backstop,
        Edge,
        Cordon,
        Lift,
    }
    fn sym_strategy() -> impl Strategy<Value = Sym> {
        prop_oneof![
            Just(Sym::Enqueue),
            Just(Sym::Enqueue),
            Just(Sym::SeatDone),
            Just(Sym::Backstop),
            Just(Sym::Edge),
            Just(Sym::Cordon),
            Just(Sym::Lift),
        ]
    }
    /// Apply one symbol to a driven host; returns the (enqueued, completed)
    /// deltas for the ledger. A `SeatDone` with nothing in flight is a legal
    /// no-op: a stray completion names a slot that does not exist and the
    /// real discipline would panic (T5), so the property skips it.
    fn apply_sym(driven: &mut DrivenHost, owner: &'static PostureOwner, sym: Sym) -> (u64, u64) {
        match sym {
            Sym::Enqueue => {
                let unit = driven.next_unit();
                driven.apply_and_pump(HostMsg::Enqueue(unit));
                (1, 0)
            }
            Sym::SeatDone => {
                driven.absorb_new_grants();
                if driven.in_flight.is_empty() {
                    (0, 0)
                } else {
                    driven.complete_oldest_in_flight();
                    (0, 1)
                }
            }
            Sym::Backstop => {
                driven.pump_handle().pump();
                (0, 0)
            }
            Sym::Edge => {
                driven.apply_and_pump(HostMsg::PostureEdge);
                (0, 0)
            }
            Sym::Cordon => {
                if owner.current() == FleetPosture::Nominal {
                    force_cordoned(owner);
                }
                driven.pump_handle().pump();
                (0, 0)
            }
            Sym::Lift => {
                if owner.current() == FleetPosture::Cordoned {
                    lift_cordon(owner);
                }
                driven.pump_handle().pump();
                (0, 0)
            }
        }
    }
    proptest! {
        /// T7 — S: `submitted == in-flight + completed + queued + backlog`
        /// after every transition, over arbitrary symbol sequences.
        #[test]
        fn safety_ledger_is_conserved_over_symbol_sequences(
            syms in prop::collection::vec(sym_strategy(), 0..40),
        ) {
            let owner = hermetic_owner();
            let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
            let mut submitted: u64 = 0;
            let mut completed: u64 = 0;
            for (i, sym) in syms.iter().enumerate() {
                let (e, c) = apply_sym(&mut driven, owner, *sym);
                submitted += e;
                completed += c;
                driven.absorb_new_grants();
                let queued = driven.host.queue_len(driven.role()) as u64;
                let backlog = driven.backlog.len() as u64;
                let in_flight = driven.in_flight.len() as u64;
                let mut grants = driven.seated_units();
                grants.sort_unstable();
                grants.dedup();
                prop_assert_eq!(
                    submitted,
                    in_flight + completed + queued + backlog,
                    "S (ledger) broken at step {} of {:?}: submitted={} inflight={} completed={} queued={} backlog={}",
                    i, syms, submitted, in_flight, completed, queued, backlog
                );
                prop_assert_eq!(
                    grants.len() as u64,
                    in_flight + completed,
                    "S (grants) broken at step {} of {:?}",
                    i, syms
                );
            }
        }
        /// T7 — L: every reachable state whose backlog the pure predicate
        /// ADMITS shrinks under ONE BackstopTick pump, with no
        /// enqueue/completion hint. FALSIFICATION: the printed admission
        /// tuple + backlog depth at which the pump was a no-op.
        #[test]
        fn liveness_backstop_tick_drains_any_admitted_backlog(
            syms in prop::collection::vec(sym_strategy(), 0..20),
        ) {
            let owner = hermetic_owner();
            let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
            for sym in &syms {
                let _ = apply_sym(&mut driven, owner, *sym);
            }
            driven.absorb_new_grants();
            if !driven.backlog.is_empty() {
                let inputs = driven.pump_handle().admission_inputs();
                let admitted = admission(inputs).admits();
                let before = driven.backlog.len();
                driven.pump_handle().pump();
                if admitted {
                    prop_assert!(
                        driven.backlog.len() < before,
                        "L broken: admitted backlog did not shrink under one BackstopTick \
                         (inputs={:?}, backlog_before={}, backlog_after={}, sequence={:?})",
                        inputs, before, driven.backlog.len(), syms
                    );
                }
            }
        }
    }
    /// T7 — "`BackstopTick` ALONE is sufficient to drain any reachable
    /// admissible backlog": enumerate backlog depths, park under a
    /// recoverable cordon, LIFT with no message, then run ONE bare pump.
    /// FALSIFICATION: a backlog still non-empty after the pump, or a unit
    /// seated twice.
    #[test]
    fn backstop_tick_alone_drains_an_admissible_backlog() {
        for backlog in 1..=3usize {
            let owner = hermetic_owner();
            let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
            force_cordoned(owner);
            for _ in 0..backlog {
                let unit = driven.next_unit();
                driven.apply_and_pump(HostMsg::Enqueue(unit));
            }
            assert_eq!(driven.backlog.len(), backlog, "held under the cordon");
            lift_cordon(owner);
            // NO Enqueue and NO SeatDone — the backstop tick alone.
            driven.pump_handle().pump();
            assert!(
                driven.backlog.is_empty(),
                "backlog of {backlog} not drained by a single backstop pump"
            );
            driven.absorb_new_grants();
            let mut units = driven.seated_units();
            units.sort_unstable();
            units.dedup();
            assert_eq!(units.len(), backlog, "every held unit seated exactly once");
        }
    }
    /// T7 — exhaustive small-bound reachability: over (backlog 0..=2) ×
    /// (posture Nominal/Cordoned), a non-empty non-Faulted backlog ALWAYS
    /// has an enabled transition — the pure predicate admits a drain, or
    /// `progress() == Backed` arms the `BackstopTick`. FALSIFICATION: a parked
    /// state with both false (the 9,955 zero-transition trap).
    #[test]
    fn no_reachable_non_faulted_backlog_has_zero_enabled_transitions() {
        for backlog in 0..=2usize {
            for cordon in [false, true] {
                let owner = hermetic_owner();
                let mut driven = DrivenHost::boot_with(HostKind::Pooled, owner);
                if cordon {
                    force_cordoned(owner);
                }
                for _ in 0..backlog {
                    let unit = driven.next_unit();
                    driven.apply_and_pump(HostMsg::Enqueue(unit));
                }
                let held = !driven.backlog.is_empty();
                let progress = driven.pump_handle().progress();
                assert_ne!(
                    progress,
                    ProgressState::Faulted,
                    "no lane death was recorded (backlog={backlog}, cordon={cordon})"
                );
                if !held {
                    assert_eq!(progress, ProgressState::Idle, "empty backlog is Idle");
                    continue;
                }
                assert_eq!(progress, ProgressState::Backed, "a held backlog is Backed");
                let admits = admission(driven.pump_handle().admission_inputs()).admits();
                // `Backed` arms the backstop (run() blocks on recv_timeout
                // iff Backed), so the enabled-transition disjunction is total.
                assert!(
                    admits || progress == ProgressState::Backed,
                    "zero enabled transitions: backlog={backlog} cordon={cordon}"
                );
            }
        }
    }
}
