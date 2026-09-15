//! `seat_host` — the ONE host machinery for the fleet hosts: the
//! byte-identical `WorkQueue` pair, the `seat_loop` pair, and — since
//! 6HE6RF — the ONE [`HostPump`] host-message triple (`apply_host_msg` +
//! the backlog-draining grant pump + the recv loop) that ALL THREE fleet
//! hosts run (the pooled pair AND the solve host), plus the
//! construction-stamped boot install/global boilerplate.
//! `fleet_sim_executor`, `fleet_registration_executor`, and
//! `fleet_solve_executor` parameterize the triple; the seat models stay
//! per host kind (P-RZEWTX): the pooled `WorkQueue` here, the solve
//! host's per-seat keyed mailboxes there.
//!
//! # The admission model (design gate — decided BEFORE any code moved)
//!
//! All three fleet roles reconcile under ONE invariant — the never-drop
//! ledger (design doc §10): no admission path drops or refuses a unit. A
//! full per-role queue and a cordon hold BOTH convert to a wait (spill
//! into the unbounded host backlog, drain FIFO); the only failure shape is
//! a closed host channel, and that is a loud abort (a lost unit strands
//! its receipt pipe). What differs per role is the cordon effect at
//! admission and the seat model:
//!
//! - registration (`PoolStateUpdater`, Deferrable cordon class): the
//!   never-drop flood with unbounded backlog; a `Cordoned` posture HOLDS
//!   new intake — held units wait in the backlog, never dropped
//!   (`fleet_intake.rs`; test
//!   `fleet_intake_facade_preserves_the_never_drop_flood`, and JCI2FW
//!   Part A's reached-Cordoned test
//!   `a_cordoned_posture_holds_registration_intake_until_the_cordon_
//!   lifts`).
//! - sim (`SimDriver`, `SimPool` cordon class): seat-pool pacing — the seat
//!   pool IS the grant lane, capping concurrent executing sims at the
//!   budget's sim slot cap; a `Cordoned` posture still ADMITS sim leases
//!   (floored, not held — the floor is the host FSM's sim-intake lane,
//!   dispatcher-side; tests `sim_seat_pool_is_the_budget_sim_slot_cap`,
//!   `concurrent_sims_are_bounded_by_the_sim_slot_cap`).
//! - solve (`Solver`, `CordonClass::Never`): posture-INVARIANT admission
//!   at the TYPED submit seam — a Cordoned posture must NEVER refuse a
//!   Solver bin (`fleet_solve_executor.rs`; test
//!   `submit_in_cordoned_posture_still_admits_solver_units_and_running_
//!   units_complete`).
//!
//! DECISION (RZEWTX, narrowed by 6HE6RF): the host serves the pairwise-
//! compatible `WorkQueue` pair (sim + registration); the solve executor's
//! SEAT MODEL (per-seat mpsc mailboxes keyed by the Solver pin — T3/T6
//! warm arenas, `seat_loop(seat, rx, done)`) and its typed submit seam
//! (`Result<SubmitReceipt, SubmitError>`) stay in
//! `fleet_solve_executor.rs` — folding the seat models themselves is the
//! documented misfit. 6HE6RF executes the spike's settled WAITING answer
//! on top: the host-MESSAGE triple (`apply_host_msg` + `pump`, backlog
//! drain included) folds ONCE here as [`HostPump`], and the solve host
//! JOINS it — the cap/consult/hold rules now apply ONCE per host instead
//! of twice per executor. The posture consult is unconditional in the
//! unified shape and behavior-EXACT for solve (spike-proven; the proof
//! restated on [`HostPump`]): `admits_lease` blocks ONLY the
//! `(Cordoned, Deferrable)` pair and `Solver` is `CordonClass::Never`.
//! JCI2FW Part A dissolved the TWO-arm `CordonAdmission` descriptor
//! field entirely: admission consults the ONE shared posture owner
//! directly and derives the policy from the role's own cordon class —
//! `FleetHost::posture_admits_role` is the SAME `admits_lease`
//! predicate the dispatcher's gates consult, so Deferrable (registration)
//! intake holds while cordoned and `SimPool` (sim) leases are
//! floored-never-held, with no per-role enum arm and no hand mirror to
//! drift. (Solve's retired `HostMsg::Throttle` arm went with it — the
//! process posture owner is fed by the block pump; every host consults
//! the same owner.) Solve follows the same never-drop PRINCIPLE (unbounded
//! backlog, §10): its queue-len mirror + typed submit receipt stay at its
//! submit seam — the mirror stamps ride the unified triple through
//! [`HostPump`]'s `mirror` field.
//!
//! # Invariants preserved (behavior byte-stable)
//!
//! - Boot/census/thread names: seat threads take `WorkerRole::thread_name()`
//!   with `{n}` substituted (`work-fleet-sim-{n}` /
//!   `work-fleet-poolupd-{n}`), host threads take the descriptor's
//!   `host_thread`; the census rows self-register inside `FleetHost::boot`
//!   — the `BootRole` labels byte-stay (`fleet_simdriver_slots` /
//!   `fleet_pool_state_updater_slots`).
//! - `boot_stamp.rs` logic untouched: every install routes the ride ledger
//!   (`record_ride`) with the descriptor's [`BootRole`], exactly as before.
//! - The `fleet_intake.rs` frozen facade untouched: the port + 2 pub fns
//!   surface is unchanged, and `InnerWork = Box<dyn FnOnce>` carries NO
//!   pyo3 in any host signature (`Python::attach` stays at the pyo3 leaf).
//! - Loud aborts: every seat/host failure funnels through [`abort_executor`],
//!   whose tag (`[fleet-sim]` / `[fleet-reg]`) and stranded-pipe noun stay
//!   byte-identical per role.
//!
//! # The lane interface (FF-T3, Z2YW52 — lanes stay logical)
//!
//! Lanes are LOGICAL: the LANEWARDEN lane vocabulary names WHO owns
//! which receipts and ledger writes, never which thread runs them — the
//! BINDING is the adapter that maps lanes to threads (two today: pinned,
//! and the serial binding that lands with FF-T4). One lane interface,
//! binding-independent by construction:
//!
//! - **H (reserve)** — the pump-driven host lane: the block pump feeds
//!   the ONE posture owner and the stage machine; it owns no fleet
//!   receipts of its own (its “writes” are the stage-machine rows).
//! - **A (ambient)** — the ambient I/O runtime (`degenbot-io-rt-{n}`):
//!   pump/dispatch/delivery/pyo3-async work; no per-unit receipts.
//! - **R (resolve)** — the pooled resolve seats (dispatcher lane): the
//!   resolve units' completions ride the slot FSM (T5), no caller pipes.
//! - **M (merge)** — the merge sidecar's per-path result pipe: EVERY
//!   solved/suppressed/failed path's terminal send lands here — the
//!   QR3NUS/LW-T7 exactness fuse (solved + suppressed + failed ==
//!   submitted) is enforced at the merge drain, per cycle.
//! - **`PoolStateUpdater` seats** — the registration intake: each unit
//!   owns its intake receipt (the awaiting caller's join), held in the
//!   unbounded §10 backlog under a cordon (never dropped).
//! - **`SimDriver` seats** — the inline sims: each request owns its
//!   per-request receipt channel (the walker's `PendingSim`), admitted
//!   under the cordon sim-intake floor.
//! - **Solver seats** — the keyed bins: each bin's per-path result
//!   sends feed the merge pipe through the lane witness (`SolveLane`:
//!   the per-cycle one-outcome-per-path ledger; a panicked bin's
//!   undelivered pids arrive as typed `Failed` records).
//!
//! The outcome ledger, intake receipts, and the exactness fuse live at
//! the LANE level: a binding changes WHICH THREADS run the lanes, never
//! the ownership. The census `binding` field prints the mapping per
//! entry (`pinned` = dedicated seat threads, `shared` = pooled runtimes,
//! `logical` = a lane riding other threads' time).
use crate::arb_engine::boot_stamp::{BootRole, BootStamp};
use crate::arb_engine::fleet_intake::InnerWork;
use crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor;
use crate::arb_engine::fleet_sim_executor::FleetSimExecutor;
use crate::arb_engine::fleet_wake;
use degenbot_core::op_error;
use degenbot_workers::budget::FleetBudget;
use degenbot_workers::dispatcher::{
    BootError, EnqueueError, FleetBoot, FleetHost, Grant, GrantKind, Unit,
};
use degenbot_workers::lane::LaneCtx;
use degenbot_workers::role::WorkerRole;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;
/// The role descriptor: everything that differs between the two pooled
/// `WorkQueue` executors, and nothing else. The executors are THIN over
/// this — the machinery (queue, seat loop, host loop, admission, boot
/// boilerplate) lives once, here. (JCI2FW Part A dissolved the per-role
/// `cordon` descriptor field; admission derives from the role's own
/// `cordon_class()` plus the ONE shared posture owner, read through
/// `FleetHost::posture_admits_role`.)
pub(crate) struct SeatRoleDesc {
    /// The pooled [`WorkerRole`] — drives the seat thread names
    /// (`thread_name()`), the per-role queue len/cap keys, and the
    /// [`Unit`] role stamp.
    pub role: WorkerRole,
    /// The one dispatch [`GrantKind`] this executor produces — the
    /// grant-shape invariant check in [`pump`] (anything else is a broken
    /// host contract, not a drop).
    pub grant: GrantKind,
    /// The [`BootRole`] ledger row this executor's boot rides
    /// (`boot_stamp::record_ride`).
    pub boot_role: BootRole,
    /// Loud-abort log tag (`"[fleet-sim]"` / `"[fleet-reg]"`) — the
    /// process's last message keeps the per-role tag byte-identical.
    pub abort_tag: &'static str,
    /// Work-noun for the abort contexts and the seat panic log
    /// (`"sim"` / `"intake"`): `"{noun} seat spawn"`,
    /// `"{noun} enqueue"`, `"{noun} unit panicked"`,
    /// `"stranded {noun} receipt pipe"`, ...
    pub noun: &'static str,
    /// The host thread's census name
    /// (`"work-fleet-sim-host"` / `"work-fleet-poolupd-host"`).
    pub host_thread: &'static str,
    /// The YI5NGB stamp-missing panic message (the F1 loud construction
    /// contract — the materializer must abort, never fall back silently).
    pub stamp_missing: &'static str,
    /// The queue-cap source: the budget field this role's seat pool sizes
    /// from (`sim_slot_cap` / `pool_state_updater_slots`).
    pub seats: fn(&FleetBudget) -> usize,
}
/// Host-bound message — ONE shape for all three fleet hosts (6HE6RF): a
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
/// The complete, constructible input tuple of the admission predicate
/// (adversarial-review requirement 5): [`admission`] reads NOTHING else —
/// no shared map, no channel depth, no host borrow. Property tests
/// construct these tuples directly with no live host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionInputs {
    /// The bounded role queue's current occupancy (`FleetHost::queue_len`).
    pub(crate) queue_len: usize,
    /// The role's queue cap (`FleetHost::queue_cap`).
    pub(crate) queue_cap: usize,
    /// The live posture consult (`FleetHost::posture_admits_role`).
    pub(crate) posture_admits: bool,
}
/// The admission predicate's total output. Saturated and posture-held are
/// NOT progress states — they are the two independent WAIT reasons here,
/// and they can co-occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The backlog head may move to the role queue.
    Admit,
    /// Blocked by the cap alone.
    WaitCap,
    /// Blocked by the posture hold alone.
    WaitPosture,
    /// Blocked by both (the terms co-occur).
    WaitBoth,
}
impl Admission {
    /// Whether the unit may leave the backlog for the role queue.
    pub(crate) fn admits(self) -> bool {
        matches!(self, Admission::Admit)
    }
}
/// THE pure admission predicate (JCI2FW unified consult, 6HE6RF fold): a
/// total function of [`AdmissionInputs`] ALONE. It is the ONLY gate on the
/// backlog → role-queue move; `try_enqueue`'s `PostureHeld` hand-back
/// stays the TOCTOU backstop for a cordon onset between the consult and the
/// enqueue (hold, never drop, never abort).
pub(crate) fn admission(inputs: AdmissionInputs) -> Admission {
    let at_cap = inputs.queue_len >= inputs.queue_cap;
    match (at_cap, !inputs.posture_admits) {
        (false, false) => Admission::Admit,
        (true, false) => Admission::WaitCap,
        (false, true) => Admission::WaitPosture,
        (true, true) => Admission::WaitBoth,
    }
}
/// The host intake backstop interval (T2): the recv-timeout armed iff a
/// backlog exists. Read from the installed typed config (schema defaults
/// when none installed); clamped to >= 1 ms so a garbage value can never
/// collapse into a busy-spin.
pub(crate) fn intake_backstop() -> Duration {
    Duration::from_millis(
        crate::bot_core::stance::config()
            .fleet
            .intake_backstop_ms
            .max(1),
    )
}
/// The no-progress trip threshold K (T4): read from the installed typed
/// config, clamped to >= 1 so a garbage value cannot trip on the first
/// pass.
pub(crate) fn intake_no_progress_ticks() -> usize {
    crate::bot_core::stance::config()
        .fleet
        .intake_no_progress_ticks
        .max(1)
}
/// The T4 no-progress guard: K CONSECUTIVE admitted-but-no-progress pump
/// passes trip the loud fail. A pass is "admitted" iff the backlog is
/// non-empty AND the pure admission predicate admits (so a legitimate
/// `WaitCap`/`WaitPosture` hold never accrues), and "progressed" iff the
/// backlog shrank or a grant was delivered. Only real progress (or an
/// un-admitted pass) resets the counter — an untrusted `PostureEdge` hint
/// runs no different code path, so it can never mask a livelock.
#[derive(Debug)]
pub(crate) struct NoProgressGuard {
    consecutive: usize,
    limit: usize,
}
/// One guard step's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoProgressStep {
    /// Progress was made, or the pass was legitimately un-admitted.
    Progressed,
    /// No progress this pass; `consecutive` of `limit` accrued.
    Ticking { consecutive: usize },
    /// K consecutive no-progress passes — the caller fails loudly.
    Tipping { consecutive: usize },
}
impl NoProgressGuard {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            consecutive: 0,
            limit: limit.max(1),
        }
    }
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }
    #[cfg(test)]
    pub(crate) fn consecutive(&self) -> usize {
        self.consecutive
    }
    /// Fold one pump pass into the guard.
    pub(crate) fn step(&mut self, admitted: bool, progressed: bool) -> NoProgressStep {
        if !admitted || progressed {
            self.consecutive = 0;
            return NoProgressStep::Progressed;
        }
        self.consecutive += 1;
        if self.consecutive >= self.limit {
            NoProgressStep::Tipping {
                consecutive: self.consecutive,
            }
        } else {
            NoProgressStep::Ticking {
                consecutive: self.consecutive,
            }
        }
    }
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
/// The serial binding's named cycle seat (FF-T4): the ONE thread that
/// runs the role's granted units in grant order on a 2-5 core host.
pub(crate) const SERIAL_SEAT_NAME: &str = "work-fleet-serial-0";
/// One granted unit handed to whichever pooled seat takes it next (both
/// roles are pooled — seats contend, no pin affinity).
struct SeatJob {
    /// The host-tracked slot the unit was granted to (completion carries it
    /// back so T5 applies to the right slot).
    slot: u64,
    /// The work payload (`Send + 'static`). Takes the seat's `LaneCtx`
    /// (LW-T2); pooled seats hand the detached stub (LW-T8 landed: the
    /// executors submit through ONE seam). NO pyo3 type crosses this seam —
    /// `Python::attach` stays at the pyo3 leaf.
    work: Box<dyn FnOnce(&LaneCtx) + Send>,
}
/// The shared pooled-seat work queue (std `mpsc` receivers are not
/// `Clone`, so the contended seat pool rides a condvar deque). Folded once
/// from the byte-identical pair (RZEWTX).
#[derive(Default)]
struct WorkQueue {
    queue: parking_lot::Mutex<VecDeque<SeatJob>>,
    shutdown: parking_lot::Mutex<bool>,
    work_available: parking_lot::Condvar,
}
impl WorkQueue {
    fn new() -> Self {
        Self::default()
    }
    /// Take one granted unit, parking the seat until one arrives or the
    /// queue shuts down (host retired — process teardown).
    fn take(&self) -> Option<SeatJob> {
        if let Some(job) = self.queue.lock().pop_front() {
            return Some(job);
        }
        let mut shutdown = self.shutdown.lock();
        loop {
            if *shutdown {
                return None;
            }
            {
                let mut q = self.queue.lock();
                if let Some(job) = q.pop_front() {
                    return Some(job);
                }
            }
            // Park until a grant lands or the host retires the pool. The
            // shutdown mutex doubles as the re-check serialization point.
            self.work_available
                .wait_for(&mut shutdown, std::time::Duration::from_millis(50));
        }
    }
    fn push(&self, job: SeatJob) {
        self.queue.lock().push_back(job);
        self.work_available.notify_one();
    }
    /// Retire the pool (host thread done): every parked seat drains out.
    fn close(&self) {
        *self.shutdown.lock() = true;
        self.work_available.notify_all();
    }
}
/// Which dispatch grant kinds a host's [`HostPump`] is contracted to serve
/// (the row-#6 contract check, loud on ALL THREE hosts since 6HE6RF): a
/// grant outside this set is a broken host contract, never a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantContract {
    /// A pooled host serves exactly ONE grant kind — the descriptor's
    /// (`GrantKind::Sim` / `GrantKind::PoolStateUpdate`).
    Single(GrantKind),
    /// The solve host serves the Solver pair: a new pin claim (T1 keyed
    /// lease) or the SAME pin's continuation (T6) — both route to the
    /// grant slot's keyed seat.
    SolverPins,
}
impl GrantContract {
    /// The pure contract predicate (the test-declared tripwire's seam —
    /// the abort itself is a process abort, untestable in-process).
    #[must_use]
    pub(crate) fn admits(self, kind: GrantKind) -> bool {
        match self {
            Self::Single(expected) => kind == expected,
            Self::SolverPins => {
                matches!(kind, GrantKind::NewPinClaim | GrantKind::PinContinuation)
            }
        }
    }
}
/// P-RZEWTX: the seat-model split of the unified host-message triple. The
/// triple (admission, backlog drain, grant loop) is ONE shape; routing a
/// GRANTED unit to its seat stays per host kind — the pooled `WorkQueue`
/// condvar pair vs the solve host's per-seat keyed mailboxes (warm arenas,
/// typed `LaneCtx`). Folding the seat models themselves is the RZEWTX
/// misfit; this trait is the seam that keeps them apart.
pub(crate) trait SeatSink {
    /// Route one T2-started grant to its seat. `host` is handed back
    /// because the solve sink mints the grant slot's warm arena
    /// (`ensure_arena`) at the same seam the pre-fold loop did.
    fn deliver(&self, host: &mut FleetHost, grant: Grant, unit: Unit);
}
/// The pooled seat model (sim + registration, RZEWTX byte-identical): the
/// granted unit joins the shared `WorkQueue` — any idle pooled seat takes
/// it (seats contend, no pin affinity); completion carries the granted
/// slot back for T5.
pub(crate) struct PooledSink {
    queue: Arc<WorkQueue>,
}
impl SeatSink for PooledSink {
    fn deliver(&self, _host: &mut FleetHost, grant: Grant, unit: Unit) {
        self.queue.push(SeatJob {
            slot: grant.slot,
            work: unit.work,
        });
    }
}
/// The per-host loud-abort + wording seam of the unified triple: every
/// context/message string stays owned by its host module so the pre-fold
/// abort wording stays byte-identical (the pooled pair's strings ride the
/// descriptor; solve's are its own). The unified code calls the SEMANTIC
/// operations only.
pub(crate) trait HostDiscipline {
    /// Unrecoverable host failure at a SHARED context ("backlog drain",
    /// "grant start (T2)") — never returns.
    fn fail(&self, context: &str, err: &str) -> !;
    /// The apply-time enqueue refusal (pooled: `"{noun} enqueue"`;
    /// solve: `"solver enqueue"`).
    fn enqueue_refused(&self, err: &str) -> !;
    /// The `SeatDone` completion refusal (pooled: `"seat completion (T5)"`;
    /// solve: `"seat completion (T3)"`).
    fn completion_refused(&self, err: &str) -> !;
    /// The row-#6 grant-kind contract denial (context `"dispatch grant"`).
    fn foreign_grant(&self, kind: GrantKind) -> !;
}
/// The pooled hosts' discipline: the descriptor owns the tag + noun, so
/// every abort string is the pre-fold byte-identical one.
pub(crate) struct PooledDiscipline {
    desc: &'static SeatRoleDesc,
}
impl HostDiscipline for PooledDiscipline {
    fn fail(&self, context: &str, err: &str) -> ! {
        abort_executor(self.desc, context, err)
    }
    fn enqueue_refused(&self, err: &str) -> ! {
        abort_executor(self.desc, &format!("{} enqueue", self.desc.noun), err)
    }
    fn completion_refused(&self, err: &str) -> ! {
        abort_executor(self.desc, "seat completion (T5)", err)
    }
    fn foreign_grant(&self, _kind: GrantKind) -> ! {
        abort_executor(
            self.desc,
            "dispatch grant",
            &format!(
                "non-{} grant in the {} executor",
                self.desc.noun, self.desc.noun
            ),
        )
    }
}
/// THE one host-message/waiting shape (6HE6RF): `apply_host_msg` + the
/// backlog-draining grant pump + the recv loop, folded ONCE behind all
/// three fleet hosts. The pooled pair (sim + registration) and the solve
/// host parameterize it — everything the JCI2FW-diverged pair actually
/// differed on is a field:
///
/// - [`SeatSink`] — the seat model split (P-RZEWTX): the pooled
///   `WorkQueue` vs the solve host's per-seat keyed mailboxes.
/// - `mirror: Option<&AtomicUsize>` — the typed-submit receipt's
///   advisory stamp (solve only; `None` on the fire-and-forget pooled
///   port). It stores the BOUNDED role-queue length at the last stamp
///   (spill or `SeatDone`) — NOT the backlog depth: the receipt bit means
///   "the role queue was >= cap at the last stamp", an advisory lagging
///   flag exactly as `SubmitReceipt`'s doc says. (6HE6RF fixed the
///   overstated "backlog mirror" COMMENT; the mechanism is kept.)
/// - [`GrantContract`] — the row-#6 grant-kind contract, now explicit on
///   all three hosts (solve GAINED it: pre-fold a foreign grant reached
///   the solve pump only to die by the `seats.get(slot)` indexing
///   accident — or, under Nominal, to be silently SEATED on a Solver
///   seat; the pooled side aborted loudly).
/// - [`HostDiscipline`] — the per-host abort wording (byte-pinned).
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
/// — the consult's unconditional presence here IS the point (one shape,
/// one consult, class-derived).
///
/// # Wake discipline (the ratified contract — TB4QGX, ADR-044)
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
    /// The seat model (P-RZEWTX split).
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
    /// The S2 fault watch (TB4QGX T6): set on entering Faulted so every
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
        // Faulted is a PURE function of the sticky typed latch (TB4QGX T6):
        // no separate flag, so double lane-death delivery is idempotent by
        // construction and no elapsed-time heuristic can reach it. Gated on
        // a fault watch: only receipt-owning hosts Fault — a SHARED process
        // posture owner's sticky latch must never fault a host with no
        // receipts (the 7KAPBB cross-test contamination class).
        if self.fault.is_some() && self.host.lane_death_held() {
            ProgressState::Faulted
        } else if self.backlog.is_empty() {
            ProgressState::Idle
        } else {
            ProgressState::Backed
        }
    }
    /// Publish the live backlog depth to the `degenbot_fleet_intake_backlog`
    /// gauge (TB4QGX T7). Called at every pump exit and after a fault drain,
    /// so a stalled held backlog is observable.
    fn publish_backlog(&self) {
        if let Some(pipeline) = crate::instruments::pipeline() {
            pipeline.set_intake_backlog(self.role.label(), self.backlog.len() as u64);
        }
    }
    /// The Faulted arm (TB4QGX T6): drain the backlog AND the role queue and
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
                // (JCI2FW Part A), unconditionally. For Solver this is
                // provably a no-op (the struct doc's proof:
                // `admits_lease` blocks only `(Cordoned, Deferrable)`;
                // `Solver` is `CordonClass::Never`) — the consult's
                // presence is the unified shape, not a new gate.
                if !admission(self.admission_inputs()).admits() {
                    self.backlog.push_back(unit);
                    if let Some(mirror) = self.mirror {
                        // The spill stamp: the receipt's advisory reads the
                        // BOUNDED role-queue length at the stamp — not the
                        // backlog depth (the honest mirror note, 6HE6RF).
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
        // Faulted FIRST (TB4QGX T6): the sticky lane-death latch means no
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
                // Row #6, loud on ALL THREE hosts since 6HE6RF: this host
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
/// The executor-facing half of the shared seat host: the host channel's
/// submit end + the unit sequence, stamped with the role descriptor.
pub(crate) struct SeatHost {
    desc: &'static SeatRoleDesc,
    tx: mpsc::Sender<HostMsg>,
    /// The waker-fan-out token (T3): deregistered on drop.
    waker: u64,
    unit_seq: AtomicU64,
    /// The resolved lane-to-thread binding (FF-T4 — test-facing
    /// assertions; the host itself moved into the host thread).
    #[cfg(test)]
    binding: degenbot_workers::plan::Binding,
    /// Test-facing seat count (the role's budget slot cap).
    #[cfg(test)]
    seats: usize,
}
impl Drop for SeatHost {
    fn drop(&mut self) {
        fleet_wake::deregister(self.waker);
    }
}
impl SeatHost {
    /// Boot the shared pooled-seat host for `desc`: boot the [`FleetHost`],
    /// spawn the pooled seat threads (the budget's per-role slot cap — the
    /// descriptor's queue-cap source — with the role's census naming), and
    /// run the dispatch loop on the host thread. Fail-loud (the typed
    /// [`BootError`]) when the declared shares cannot host the quota.
    ///
    /// # Errors
    /// [`BootError`] — the fleet budget sum check or a boot invariant.
    pub(crate) fn boot(
        desc: &'static SeatRoleDesc,
        boot: FleetBoot,
        fault: Option<std::sync::Arc<crate::arb_engine::fleet_intake::IntakeFaultWatch>>,
    ) -> Result<Self, BootError> {
        let host = FleetHost::boot(boot)?;
        // FF-T3 (Z2YW52): the LANE-TO-THREAD BINDING SEAM — one lane
        // interface, the binding is the adapter that maps lanes to
        // threads (the second adapter at the Executor seam; the
        // two-adapter rule makes the seam real). The PINNED binding
        // preserves today's topology exactly (the pooled WorkQueue seat
        // model over the ONE HostPump); the serial binding — a SeatSink
        // + ONE grant lane over the SAME HostPump, not a new lane
        // interface — lands with FF-T4: until then the arm refuses
        // here with the plan's typed pending refusal (never a silent
        // narrow).
        match host.plan().binding {
            degenbot_workers::plan::Binding::Pinned => Ok(Self::boot_pinned(desc, host, fault)),
            // FF-T4 (Z6XTDX): the serial arm BOOTS — one named cycle
            // thread (`work-fleet-serial-0`) runs the role's FSM slots in
            // grant order over the SAME queue and HostPump (§10
            // never-drop unchanged; the layout seats stay the FSM's
            // slots — the census rows print `logical`, riding the cycle
            // lane's time).
            degenbot_workers::plan::Binding::Serial => Ok(Self::boot_serial(desc, host, fault)),
        }
    }
    /// The PINNED binding's instantiation (today's topology, verbatim:
    /// the pooled seat threads over the shared `WorkQueue`, the host thread
    /// running the ONE `HostPump` triple). Behavior-identical by
    /// construction — the parity corpus (the LW-T7 golden replay +
    /// the executor suites) is the regression harness.
    fn boot_pinned(
        desc: &'static SeatRoleDesc,
        host: FleetHost,
        fault: Option<std::sync::Arc<crate::arb_engine::fleet_intake::IntakeFaultWatch>>,
    ) -> Self {
        #[cfg(test)]
        let binding = host.plan().binding;
        let seats = (desc.seats)(host.budget());
        let (tx, rx) = mpsc::channel::<HostMsg>();
        // Pooled seats contend on ONE shared work queue: a grant lands a
        // unit there, any idle seat takes it, and the completion reports
        // the GRANTED slot id so the host applies T5 to the right slot.
        // Grants never exceed the role's slot cap, which never exceeds the
        // seat count, so every granted unit is picked up without delay.
        let work = Arc::new(WorkQueue::new());
        for seat in 0..seats {
            let done = tx.clone();
            let work = Arc::clone(&work);
            let spawned = std::thread::Builder::new()
                .name(desc.role.thread_name().replace("{n}", &seat.to_string()))
                .spawn(move || seat_loop(desc, &work, &done));
            if let Err(err) = spawned {
                // A missing seat strands the receipts of every unit that
                // would have run on it — loud (§10).
                abort_executor(
                    desc,
                    &format!("{} seat spawn", desc.noun),
                    &format!("{err:?}"),
                );
            }
        }
        let spawned = std::thread::Builder::new()
            .name(desc.host_thread.to_string())
            .spawn(move || {
                host_loop(rx, host, desc, Arc::clone(&work), fault);
                // Process teardown: the submission channel closed. Retire
                // the seats so no worker parks forever on an empty queue.
                work.close();
            });
        if let Err(err) = spawned {
            abort_executor(
                desc,
                &format!("fleet {} host thread spawn", desc.noun),
                &format!("{err:?}"),
            );
        }
        Self {
            desc,
            waker: fleet_wake::register(&tx),
            tx,
            unit_seq: AtomicU64::new(0),
            #[cfg(test)]
            binding,
            #[cfg(test)]
            seats,
        }
    }
    /// The SERIAL binding's instantiation (FF-T4): the SAME pooled
    /// queue, host loop, and FSM slots — but ONE named cycle thread
    /// (`work-fleet-serial-0`, the serial seat) runs every granted unit
    /// in grant order: the role's lane rides the cycle lane's time
    /// (the census prints `logical`). Intake stays the §10 never-drop
    /// shape; saturation is the advisory queue depth, named and metered
    /// through the census's serial binding row (no second waiting
    /// policy — the 6HE6RF amendment).
    fn boot_serial(
        desc: &'static SeatRoleDesc,
        host: FleetHost,
        fault: Option<std::sync::Arc<crate::arb_engine::fleet_intake::IntakeFaultWatch>>,
    ) -> Self {
        #[cfg(test)]
        let binding = host.plan().binding;
        #[cfg(test)]
        let seats = (desc.seats)(host.budget());
        let (tx, rx) = mpsc::channel::<HostMsg>();
        let work = Arc::new(WorkQueue::new());
        // ONE cycle thread: the serial seat. Named for the census and
        // the thread-dump reader (the station test greps it).
        let done = tx.clone();
        let work_cycle = Arc::clone(&work);
        let spawned = std::thread::Builder::new()
            .name(SERIAL_SEAT_NAME.to_string())
            .spawn(move || seat_loop(desc, &work_cycle, &done));
        if let Err(err) = spawned {
            // A missing cycle lane strands the receipts of every
            // granted unit — loud (§10).
            abort_executor(
                desc,
                &format!("{} serial cycle lane spawn", desc.noun),
                &format!("{err:?}"),
            );
        }
        let spawned = std::thread::Builder::new()
            .name(desc.host_thread.to_string())
            .spawn(move || {
                host_loop(rx, host, desc, Arc::clone(&work), fault);
                work.close();
            });
        if let Err(err) = spawned {
            abort_executor(
                desc,
                &format!("fleet {} host thread spawn", desc.noun),
                &format!("{err:?}"),
            );
        }
        Self {
            desc,
            waker: fleet_wake::register(&tx),
            tx,
            unit_seq: AtomicU64::new(0),
            #[cfg(test)]
            binding,
            #[cfg(test)]
            seats,
        }
    }
    /// Test-facing seat count (the role's budget slot cap).
    #[cfg(test)]
    pub(crate) fn seat_count(&self) -> usize {
        self.seats
    }
    /// Test-facing: the resolved plan binding (FF-T4 — the executors'
    /// tests assert the tier the boot instantiated).
    #[cfg(test)]
    pub(crate) fn plan_binding(&self) -> degenbot_workers::plan::Binding {
        self.binding
    }
    /// The port's unit body (folded from the two executors' pre-existing
    /// submit bodies): wraps into `Unit::new(.., Box::new(move |_ctx|
    /// work()))` and enqueues over `tx.send(HostMsg::Enqueue(unit))`, typed
    /// to the port's `Result<(), ()>` close vocabulary — the send VALUE
    /// carries the close arm; the abort lives in [`intake_spawn`] (same
    /// process-exit semantics, one owner of the abort).
    pub(crate) fn try_send(&self, work: InnerWork) -> Result<(), ()> {
        let unit = Unit::new(
            self.unit_seq.fetch_add(1, Ordering::Relaxed),
            self.desc.role,
            None,
            // The unit's receipt feeds the awaiting caller's join — a
            // stranded pipe if abandoned.
            true,
            Box::new(move |_ctx| work()),
        );
        // The close arm, typed to the port's unit vocabulary: the send
        // value carries the close arm; the abort lives in the trait impl.
        match self.tx.send(HostMsg::Enqueue(unit)) {
            Ok(()) => Ok(()),
            Err(_) => Err(()),
        }
    }
}
/// The [`crate::arb_engine::fleet_intake::FleetIntake`] port's close arm,
/// shared by both executors' trait impls: a closed host channel (the host
/// thread died) is a LOUD abort — a lost unit strands its receipt pipe
/// (§10). One owner of the per-role close strings; the impls keep the
/// original `if self.try_send(work).is_err()` shape.
pub(crate) fn intake_close_abort(host: &SeatHost) -> ! {
    abort_executor(
        host.desc,
        &format!("{} submission", host.desc.noun),
        &format!("fleet {} host channel closed", host.desc.noun),
    );
}
/// One pooled seat: take granted units from the shared work queue, run
/// them one at a time, and report the granted slot's completion so the
/// host applies T5 (run → idle).
fn seat_loop(desc: &'static SeatRoleDesc, work: &WorkQueue, done: &mpsc::Sender<HostMsg>) {
    while let Some(job) = work.take() {
        // A panicking unit closure must not kill the seat (its pool would
        // strand receipts): keep the seat alive, log loudly, report done.
        let ctx = LaneCtx::detached();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| (job.work)(&ctx)));
        if outcome.is_err() {
            op_error!(
                domain = solver,
                seat = job.slot,
                "{} {} unit panicked — the seat survives, the failure is loud",
                desc.abort_tag,
                desc.noun
            );
        }
        if done.send(HostMsg::SeatDone { seat: job.slot }).is_err() {
            // The host is gone (executor dropped — tests): the seat retires.
            break;
        }
    }
}
/// The pooled hosts' dispatch loop: build the unified [`HostPump`] for the
/// descriptor's role (the FOLD MAP — everything per-host is a field) and
/// run the ONE recv → apply → pump loop (6HE6RF).
/// (`rx` moves into [`HostPump::run`] — the thread-boundary move the
/// pre-fold loop needed a lint expectation for is now `run`'s.)
#[expect(
    clippy::needless_pass_by_value,
    reason = "the Arc moves into the spawned host thread — a borrow cannot cross the thread boundary"
)]
fn host_loop(
    rx: mpsc::Receiver<HostMsg>,
    mut host: FleetHost,
    desc: &'static SeatRoleDesc,
    queue: Arc<WorkQueue>,
    fault: Option<std::sync::Arc<crate::arb_engine::fleet_intake::IntakeFaultWatch>>,
) {
    let sink = PooledSink { queue };
    let discipline = PooledDiscipline { desc };
    let mut backlog = VecDeque::new();
    let mut no_progress = NoProgressGuard::new(intake_no_progress_ticks());
    HostPump {
        host: &mut host,
        backlog: &mut backlog,
        role: desc.role,
        grants: GrantContract::Single(desc.grant),
        sink: &sink,
        // The pooled port is fire-and-forget (`Result<(), ()>`): there is
        // no typed receipt to inform, so no mirror.
        mirror: None,
        discipline: &discipline,
        backstop: intake_backstop(),
        no_progress: &mut no_progress,
        fault: fault.as_deref(),
        #[cfg(test)]
        ticks: None,
    }
    .run(rx);
}
/// Loud, unrecoverable executor failure (mirror of the fleet solve
/// executor's abort discipline): a dead host would strand in-flight
/// receipts — an awaiting caller parks forever (stranded pipe, design doc
/// §10) — so swallowing the error is never an option. The per-role tag and
/// stranded-pipe noun come from the descriptor, byte-identical to the
/// pre-fold messages.
#[expect(
    clippy::print_stderr,
    reason = "the abort path must stay legible with no tracing subscriber installed (test harnesses drop the tracing event); stderr is the process's last message"
)]
fn abort_executor(desc: &SeatRoleDesc, context: &str, err: &str) -> ! {
    op_error!(domain = solver, context = %context,
        error = %err,
        "{} unrecoverable — aborting (stranded {} receipt pipe)",
        desc.abort_tag,
        desc.noun
    );
    eprintln!(
        "{} UNRECOVERABLE, aborting (stranded {} receipt pipe): {context}: {err}",
        desc.abort_tag, desc.noun
    );
    std::process::abort();
}
/// Install the CONSTRUCTION-STAMPED boot (YI5NGB): the engine's own typed
/// boot descriptor (fleet quota + overrides + posture) parsed at ITS
/// construction from the CALLER cfg, stamped with the engine id + a
/// deterministic cfg hash. Never overrides an installed value (first
/// engine wins, like the other stance statics) — every construction after
/// the first RIDES, and the ride is ledgered (a divergent-cfg rider is
/// counted + warned in prod, ILLEGAL in tests) on the descriptor's
/// [`BootRole`] row.
pub(crate) fn install_boot(courier: &OnceLock<BootStamp>, desc: &SeatRoleDesc, stamp: BootStamp) {
    crate::arb_engine::boot_stamp::record_ride(desc.boot_role, &stamp);
    // FF-T5 (NT7HJC): record the resolved fleet profile ONCE (first
    // writer wins, like the stamp itself): the process summary feeds the
    // degenbot_fleet_profile metric, and a serial-tier resolution fires
    // the production alert (never a silent narrow).
    crate::arb_engine::fleet_status::record_fleet_profile_at_install(&stamp.boot());
    let _ = courier.set(stamp);
}
/// The process-wide materializer (the global_* boilerplate, folded from
/// the two executors): lazily boot the executor from the
/// construction-stamped boot and persist it for the process lifetime.
/// YI5NGB: the absence window is CLOSED BY CONSTRUCTION — every dispatch
/// path builds on a constructed engine, and construction (`with_core_cfg`)
/// installs the stamp BEFORE any dispatch can exist. A missing stamp means
/// a caller skipped the construction contract: LOUD abort (never a silent
/// fallback boot of a boot nobody chose).
///
/// FF-T1 (BPHR6F): the BOOT-REFUSAL arm is TYPED and STICKY — never a
/// process abort. A refused boot parks its `BootError` in the executor
/// slot's `OnceLock`: the first caller surfaces the typed error and every
/// later caller re-surfaces the SAME refusal. The refusal resolves
/// before any lane, thread, or pipe is created (`FleetHost::boot` derives
/// the budget FIRST — before any slot table, thread, or channel exists),
/// so a submit can never enqueue into a pipe that will not be drained.
/// The loud fail-fast exit stays BINARY-only: the degenbot binary maps
/// the surfaced `BootRefused` to its named exit. The RUNTIME strand
/// aborts (seat/host thread spawn, enqueue refusal, completion refusal,
/// the closed-channel close arm) keep `abort_executor` and their
/// byte-pinned wording — ADR-040 fatal bucket, BY DESIGN.
pub(crate) fn global_executor<T>(
    desc: &'static SeatRoleDesc,
    courier: &'static OnceLock<BootStamp>,
    slot: &'static OnceLock<Result<T, BootError>>,
    boot: fn(FleetBoot) -> Result<T, BootError>,
) -> Result<&'static T, BootError> {
    slot.get_or_init(|| {
        #[expect(
            clippy::expect_used,
            reason = "the loud construction-contract abort IS the YI5NGB design: a stamp-less materialization must abort, never fall back silently"
        )]
        let stamp = courier.get().expect(desc.stamp_missing);
        boot(stamp.boot()).inspect_err(|err| {
            // FF-T1 (BPHR6F): the BOOT-REFUSAL family is typed and sticky,
            // never a process abort — the library must never abort the host
            // process on this arm (the 2026-09-11 CI failures: the fleet
            // budget refusal SIGABRT'd pytest-xdist workers inside the
            // extension). ONE loud refusal line per process (the operator
            // record); the CALLER-facing contract is the typed BootError the
            // OnceLock hands every later submitter. Because FleetHost::boot
            // derives the budget FIRST — before any slot table, thread, or
            // channel exists — a refused boot created NOTHING: no pipe that
            // will not be drained, no lane, no thread. Same message
            // discipline as the abort family (tag + context), minus the
            // abort: the process survives; the binary owns the loud exit.
            op_error!(domain = solver, context = "fleet budget boot",
                error = %err,
                "{} fleet boot refused — typed error surfaces to the caller (FF-T1); the process survives",
                desc.abort_tag
            );
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}
// ======================================================================
// candidate 4 (DQA7YL / YUMQU3): the FleetBootRegistry.
//
// The ONE keyed owner of the two POOLED roles' boot facts. The solve host
// is deliberately absent: it has a different seat model (per-seat keyed
// mailboxes, typed receipts) and installs its own boot; candidate 4 is two
// rows, not three.
// ======================================================================
/// The sim role's seat descriptor. candidate 4 MOVED this here from
/// `fleet_sim_executor`: the registry owns the pooled-role descriptor rows
/// now; the role module owns only its executor + thin boot fn.
static SIM_ROLE: SeatRoleDesc = SeatRoleDesc {
    role: WorkerRole::SimDriver,
    grant: GrantKind::Sim,
    boot_role: BootRole::Sim,
    abort_tag: "[fleet-sim]",
    noun: "sim",
    host_thread: "work-fleet-sim-host",
    stamp_missing:
        "fleet sim boot stamp missing: an engine must construct before the first fleet submit (YI5NGB)",
    seats: sim_seats_of,
};
/// The queue-cap source: the budget's `SimDriver` slot cap.
fn sim_seats_of(budget: &FleetBudget) -> usize {
    budget.sim_slot_cap
}
/// The registration role's seat descriptor (moved here from
/// `fleet_registration_executor`, same rationale).
static REG_ROLE: SeatRoleDesc = SeatRoleDesc {
    role: WorkerRole::PoolStateUpdater,
    grant: GrantKind::PoolStateUpdate,
    boot_role: BootRole::Registration,
    abort_tag: "[fleet-reg]",
    noun: "intake",
    host_thread: "work-fleet-poolupd-host",
    stamp_missing:
        "fleet registration boot stamp missing: an engine must construct before the first fleet submit (YI5NGB)",
    seats: reg_seats_of,
};
/// The queue-cap source: the budget's `pool_state_updater_slots`.
fn reg_seats_of(budget: &FleetBudget) -> usize {
    budget.pool_state_updater_slots
}
/// The sim boot courier (the registry slot's `&'static` storage).
static SIM_BOOT: OnceLock<BootStamp> = OnceLock::new();
/// The sim executor courier (first-wins, sticky boot outcome).
static SIM_EXECUTOR: OnceLock<Result<FleetSimExecutor, BootError>> = OnceLock::new();
/// The registration boot courier.
static REG_BOOT: OnceLock<BootStamp> = OnceLock::new();
/// The registration executor courier.
static REG_EXECUTOR: OnceLock<Result<FleetRegistrationExecutor, BootError>> = OnceLock::new();
/// One typed executor slot in the [`FleetBootRegistry`]: the role
/// descriptor, the construction-stamped boot courier, and the process-wide
/// executor courier. The couriers are `&'static` references into the
/// registry's static storage, so [`BootSlot::executor`] can hand out a
/// truly `'static` reference (the process-global executor outlives every
/// caller).
pub(crate) struct BootSlot<T: 'static> {
    desc: &'static SeatRoleDesc,
    boot: &'static OnceLock<BootStamp>,
    executor: &'static OnceLock<Result<T, BootError>>,
}
impl<T: 'static> BootSlot<T> {
    const fn new(
        desc: &'static SeatRoleDesc,
        boot: &'static OnceLock<BootStamp>,
        executor: &'static OnceLock<Result<T, BootError>>,
    ) -> Self {
        Self {
            desc,
            boot,
            executor,
        }
    }
    /// The role's seat descriptor (the `BootRole`-keyed row).
    #[must_use]
    pub(crate) fn descriptor(&self) -> &'static SeatRoleDesc {
        self.desc
    }
    /// Whether an engine installed this role's construction-stamped boot.
    /// Test-facing (the pinned registry contract); production reads the
    /// registry's canonical first-wins latch.
    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub(crate) fn boot_installed(&self) -> bool {
        self.boot.get().is_some()
    }
    /// The process-wide executor, if it already materialized (the sticky
    /// boot-outcome slot: a refused boot is `Ok(None)` here, not a panic).
    /// Test-facing (the pinned registry contract).
    #[cfg_attr(not(test), expect(dead_code))]
    #[must_use]
    pub(crate) fn executor(&self) -> Option<&'static T> {
        self.executor
            .get()
            .and_then(|outcome| outcome.as_ref().ok())
    }
    /// Install the CONSTRUCTION-STAMPED boot through the ONE shared
    /// installer ([`install_boot`]): ledger the identified ride, record the
    /// first-wins fleet profile, courier the stamp. Never overrides.
    pub(crate) fn install(&self, stamp: BootStamp) {
        install_boot(self.boot, self.desc, stamp);
    }
    /// Test-only: park a boot stamp WITHOUT the shared install side effects
    /// (the FF-T1 unhostable-boot refusal fixture).
    #[cfg(test)]
    pub(crate) fn set_boot_for_test(&self, stamp: BootStamp) {
        let _ = self.boot.set(stamp);
    }
    /// The process-wide materializer (the shared [`global_executor`]
    /// boilerplate), keyed to this slot's couriers.
    ///
    /// # Errors
    /// FF-T1 (BPHR6F): the typed, sticky fleet boot refusal.
    pub(crate) fn global_executor(
        &self,
        boot: fn(FleetBoot) -> Result<T, BootError>,
    ) -> Result<&'static T, BootError> {
        global_executor(self.desc, self.boot, self.executor, boot)
    }
}
/// The uniform, [`BootRole`]-keyed view of a registry slot.
pub(crate) trait BootSlotView {
    /// The role's seat descriptor.
    fn descriptor(&self) -> &'static SeatRoleDesc;
    /// Install the role's construction boot (first-wins).
    fn install(&self, stamp: BootStamp);
}
impl<T: 'static> BootSlotView for BootSlot<T> {
    fn descriptor(&self) -> &'static SeatRoleDesc {
        BootSlot::descriptor(self)
    }
    fn install(&self, stamp: BootStamp) {
        BootSlot::install(self, stamp);
    }
}
/// The candidate-4 pooled-role boot registry: the ONE keyed owner of the
/// two pooled roles' boot facts. Each typed slot carries the role
/// descriptor + boot/executor couriers; the registry carries the first-wins
/// canonical process boot (whichever role installs first — sim before
/// registration in `lifecycle::install_engine_stances`). `fleet_status` and
/// `fleet_intake` read THIS, never a role-module static.
pub(crate) struct FleetBootRegistry {
    sim: BootSlot<FleetSimExecutor>,
    registration: BootSlot<FleetRegistrationExecutor>,
    /// The canonical process boot: the FIRST registry role to install owns
    /// it, stamped exactly once.
    process_boot: OnceLock<FleetBoot>,
}
impl FleetBootRegistry {
    /// The process registry singleton.
    #[must_use]
    pub(crate) fn process() -> &'static Self {
        static REGISTRY: OnceLock<FleetBootRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| Self {
            sim: BootSlot::new(&SIM_ROLE, &SIM_BOOT, &SIM_EXECUTOR),
            registration: BootSlot::new(&REG_ROLE, &REG_BOOT, &REG_EXECUTOR),
            process_boot: OnceLock::new(),
        })
    }
    /// The sim role's typed slot.
    #[must_use]
    pub(crate) fn sim(&self) -> &BootSlot<FleetSimExecutor> {
        &self.sim
    }
    /// The registration role's typed slot.
    #[must_use]
    pub(crate) fn registration(&self) -> &BootSlot<FleetRegistrationExecutor> {
        &self.registration
    }
    /// The `BootRole`-keyed uniform view.
    ///
    /// # Panics
    /// `BootRole::Solve` is out of registry scope: candidate 4 registers the
    /// TWO pooled roles (sim + registration); the solve host's seat model is
    /// different by design and has no slot here.
    #[must_use]
    pub(crate) fn role(&self, role: BootRole) -> &dyn BootSlotView {
        match role {
            BootRole::Sim => &self.sim,
            BootRole::Registration => &self.registration,
            BootRole::Solve => unreachable!(
                "FleetBootRegistry covers the two pooled roles (sim, registration); the solve role is out of registry scope"
            ),
        }
    }
    /// Whether ANY registry role installed its construction boot (the
    /// first-wins process latch).
    #[must_use]
    pub(crate) fn boot_installed(&self) -> bool {
        self.process_boot.get().is_some()
    }
    /// The canonical process boot — whichever registry role installed FIRST.
    /// `None` pre-construction.
    #[must_use]
    pub(crate) fn process_boot(&self) -> Option<FleetBoot> {
        self.process_boot.get().copied()
    }
    /// Install the construction-stamped boot for `role` through the ONE
    /// shared installer: the canonical process boot is first-wins, then the
    /// role slot ledgers the ride + records the first-wins profile.
    pub(crate) fn install_boot(&self, role: BootRole, stamp: BootStamp) {
        // First-wins canonical: the first registry role to install owns it.
        let _ = self.process_boot.set(stamp.boot());
        let slot = self.role(role);
        debug_assert_eq!(
            slot.descriptor().boot_role,
            role,
            "registry slot/role mismatch"
        );
        slot.install(stamp);
    }
}
/// The shared per-role submit / port / test-shim surface, defined ONCE and
/// instantiated per pooled executor. `self.$host` names the executor's
/// seat-host field at the invocation site; the `FleetIntake` port impl, the
/// inherent `try_send`, and the test-only `spawn` shim all live here — the
/// role modules no longer carry byte-identical copies.
macro_rules! impl_seat_hosted {
    ($executor:ty, $host:ident) => {
        impl $executor {
            /// The shared host's submit end (the port's `Result<(), ()>`
            /// close vocabulary).
            pub(crate) fn try_send(
                &self,
                work: $crate::arb_engine::fleet_intake::InnerWork,
            ) -> Result<(), ()> {
                self.$host.try_send(work)
            }
            /// Test-venue shim: the OLD name `spawn`, `#[cfg(test)]`-only.
            #[cfg(test)]
            fn spawn(&self, work: impl FnOnce() + Send + 'static) {
                let _ = self.try_send(Box::new(work));
            }
        }
        impl $crate::arb_engine::fleet_intake::FleetIntake for $executor {
            fn spawn(&self, work: $crate::arb_engine::fleet_intake::InnerWork) {
                if self.try_send(work).is_err() {
                    $crate::arb_engine::seat_host::intake_close_abort(&self.$host);
                }
            }
        }
    };
}
pub(crate) use impl_seat_hosted;
// ---------------------------------------------------------------------------
// 6HE6RF: the cross-host property suite. The unified HostPump is driven
// DIRECTLY in both parameterizations (the dispatcher::harness precedent —
// the core is exercised by a deterministic script, not by production
// callers), with the seat-model artifacts (seat counts, keyed-pin chain,
// mailbox routing, arena ids) normalized away by a recording sink.
// ---------------------------------------------------------------------------
#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic)]
mod tests {
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
    /// gets its own owner, never the process global (7KAPBB isolation).
    /// Per-file hermeticity stays per-file (the 6HE6RF KILL list): this
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
    /// the dirty sample at now = 0) — the JCI2FW lift.
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
            panic!("6HE6RF row-#6 denial recorded: {kind:?}");
        }
    }
    /// The two host kinds the property drives (the parameterization IS the
    /// fold map).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HostKind {
        /// The pooled pair's registration arm: `PoolStateUpdater` (Deferrable
        /// — the JCI2FW cordon hold is reachable), one grant kind, no
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
    /// THE cross-host property (6HE6RF): drive IDENTICAL unit sequences
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
    /// HOLDS intake in the backlog (JCI2FW), while the solve host ADMITS
    /// and grants under the cordon (7OGY5V — the consult is provably
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
    /// (the 6HE6RF red: no `try_enqueue` hand-back arm — the pooled host
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
    /// Q3.4 (the honest mirror, 6HE6RF): the receipt bit tracks the
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
    // ── T7 (TB4QGX): mechanized safety, liveness, reachability ────────────
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
// ======================================================================
// Pins for the candidate-4 FleetBootRegistry contract.
// Written against the TARGET contract; production code is NOT changed.
// ======================================================================
#[cfg(test)]
mod candidate4_seam_pins {
    use super::{BootSlot, BootSlotView, FleetBootRegistry, SeatRoleDesc};
    use crate::arb_engine::boot_stamp::BootRole;
    use crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor;
    use crate::arb_engine::fleet_sim_executor::FleetSimExecutor;
    /// candidate4 pin 1 - RED at HEAD (compile: `seat_host` owns no
    /// `FleetBootRegistry`). TARGET (post-T2): ONE keyed boot owner carrying
    /// a per-`BootRole` accessor (`sim()` / `registration()`) that exposes
    /// the role descriptor AND the TYPED executor slot, plus registry-level
    /// `boot_installed()` / `process_boot()` first-wins process facts.
    #[test]
    fn candidate4_registry_is_the_keyed_boot_owner() {
        type Registry = FleetBootRegistry;
        let _: fn() -> &'static Registry = Registry::process;
        // The keyed role accessor per `BootRole` returns the typed slot.
        let _: fn(&Registry) -> &BootSlot<FleetSimExecutor> = Registry::sim;
        let _: fn(&Registry) -> &BootSlot<FleetRegistrationExecutor> = Registry::registration;
        // The `BootRole`-keyed view exposes the descriptor uniformly.
        let _: fn(&Registry, BootRole) -> &dyn BootSlotView = Registry::role;
        // Registry-level first-wins process boot facts.
        let _: fn(&Registry) -> bool = Registry::boot_installed;
        let _: fn(&Registry) -> Option<degenbot_workers::dispatcher::FleetBoot> =
            Registry::process_boot;
        // The descriptor is reachable THROUGH the keyed accessor.
        let _: fn(&BootSlot<FleetSimExecutor>) -> &'static SeatRoleDesc = BootSlot::descriptor;
        // The TYPED executor slot is reachable THROUGH the keyed accessor.
        let _: fn(&BootSlot<FleetSimExecutor>) -> Option<&'static FleetSimExecutor> =
            BootSlot::executor;
    }
    /// candidate4 pin 3 - RED at HEAD (compile: the registry path is absent)
    /// and GREEN after T2. TARGET: the role modules shrink to descriptor rows
    /// + thin boot fns; the process boot/global wiring lives on the registry.
    ///
    /// DOCUMENTATION LATCH (a module-private static has no compile-observable
    /// method signature, so absence of the role-module wiring cannot be probed
    /// by method resolution - this is the explicit human half):
    /// post-T2 `fleet_sim_executor` must NOT own `static SIM_ROLE`,
    /// `static FLEET_SIM_BOOT: OnceLock<BootStamp>`, `static
    /// FLEET_SIM_EXECUTOR: OnceLock<Result<..>>`, or its own
    /// `install_boot`/`global_fleet_sim_executor`; likewise
    /// `fleet_registration_executor` must NOT own `static REG_ROLE`,
    /// `static FLEET_REGISTRATION_BOOT`, `static FLEET_REGISTRATION_EXECUTOR`,
    /// or `install_boot`/`boot_installed`/`stamped_boot`/
    /// `global_fleet_registration_executor`. The forward probe below catches
    /// any REINTRODUCTION of that wiring as an INHERENT method.
    #[test]
    fn candidate4_role_modules_are_descriptor_rows() {
        // The post-T2 registry path that REPLACES the per-role statics and
        // global wiring (RED half - compile errors until T2).
        fn sim_slot() -> &'static BootSlot<FleetSimExecutor> {
            FleetBootRegistry::process().sim()
        }
        fn registration_slot() -> &'static BootSlot<FleetRegistrationExecutor> {
            FleetBootRegistry::process().registration()
        }
        // GREEN half (forward absence probe, candidate2's `NoInherentTwinProbe`
        // pattern): if any retired per-role wiring reappears as an INHERENT
        // `FleetSimExecutor`/`FleetRegistrationExecutor` method, the probe
        // call below resolves to it and fails to compile (arity/type mismatch).
        struct WiringProbeToken;
        trait NoInherentWiringProbe {
            fn install_boot(&self, _t: WiringProbeToken);
            fn global_fleet_sim_executor(&self, _t: WiringProbeToken);
            fn global_fleet_registration_executor(&self, _t: WiringProbeToken);
            fn boot_installed(&self, _t: WiringProbeToken);
            fn stamped_boot(&self, _t: WiringProbeToken);
        }
        impl NoInherentWiringProbe for FleetSimExecutor {
            fn install_boot(&self, _t: WiringProbeToken) {}
            fn global_fleet_sim_executor(&self, _t: WiringProbeToken) {}
            fn global_fleet_registration_executor(&self, _t: WiringProbeToken) {}
            fn boot_installed(&self, _t: WiringProbeToken) {}
            fn stamped_boot(&self, _t: WiringProbeToken) {}
        }
        impl NoInherentWiringProbe for FleetRegistrationExecutor {
            fn install_boot(&self, _t: WiringProbeToken) {}
            fn global_fleet_sim_executor(&self, _t: WiringProbeToken) {}
            fn global_fleet_registration_executor(&self, _t: WiringProbeToken) {}
            fn boot_installed(&self, _t: WiringProbeToken) {}
            fn stamped_boot(&self, _t: WiringProbeToken) {}
        }
        #[expect(dead_code)]
        fn wiring_must_stay_off_the_role_modules(
            sim: &FleetSimExecutor,
            registration: &FleetRegistrationExecutor,
        ) {
            sim.install_boot(WiringProbeToken);
            sim.global_fleet_sim_executor(WiringProbeToken);
            registration.global_fleet_registration_executor(WiringProbeToken);
            registration.boot_installed(WiringProbeToken);
            registration.stamped_boot(WiringProbeToken);
        }
        let _ = sim_slot as fn() -> &'static BootSlot<FleetSimExecutor>;
        let _ = registration_slot as fn() -> &'static BootSlot<FleetRegistrationExecutor>;
    }
}
