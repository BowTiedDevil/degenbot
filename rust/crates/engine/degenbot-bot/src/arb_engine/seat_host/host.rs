//! The seat host itself (ADR-079 D1): the role descriptor, the pooled
//! `WorkQueue` seat model ([`SeatJob`], [`PooledSink`], [`PooledDiscipline`]),
//! [`SeatHost`] (the executor-facing submit end), the `seat_loop` /
//! `host_loop` pair, the loud-abort seam, and the shared
//! `impl_seat_hosted!` per-role surface.

use super::admission::{intake_backstop, intake_no_progress_ticks, GrantContract, NoProgressGuard};
use super::pump::{HostDiscipline, HostMsg, HostPump, SeatSink};
use crate::arb_engine::boot_stamp::BootRole;
use crate::arb_engine::fleet_intake::InnerWork;
use crate::arb_engine::fleet_wake;
use degenbot_core::op_error;
use degenbot_workers::budget::FleetBudget;
use degenbot_workers::dispatcher::{BootError, FleetBoot, FleetHost, Grant, GrantKind, Unit};
use degenbot_workers::lane::LaneCtx;
use degenbot_workers::role::WorkerRole;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
/// The role descriptor: everything that differs between the two pooled
/// `WorkQueue` executors, and nothing else. The executors are THIN over
/// this — the machinery (queue, seat loop, host loop, admission, boot
/// boilerplate) lives once, here. (dissolved the per-role
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
    /// The stamp-missing panic message (the F1 loud construction
    /// contract — the materializer must abort, never fall back silently).
    pub stamp_missing: &'static str,
    /// The queue-cap source: the budget field this role's seat pool sizes
    /// from (`sim_slot_cap` / `pool_state_updater_slots`).
    pub seats: fn(&FleetBudget) -> usize,
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
/// from the byte-identical pair (.
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
/// The pooled seat model (sim + registration, byte-identical): the
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
        // FF-T3: the LANE-TO-THREAD BINDING SEAM — one lane
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
            // FF-T4: the serial arm BOOTS — one named cycle
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
                let sink = PooledSink {
                    queue: Arc::clone(&work),
                };
                let discipline = PooledDiscipline { desc };
                host_loop(
                    rx,
                    host,
                    HostLoop {
                        role: desc.role,
                        grants: GrantContract::Single(desc.grant),
                        sink: &sink,
                        // The pooled port is fire-and-forget (`Result<(), ()>`):
                        // there is no typed receipt to inform, so no mirror.
                        mirror: None,
                        discipline: &discipline,
                        fault: fault.as_deref(),
                    },
                );
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
    /// policy — the amendment).
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
                let sink = PooledSink {
                    queue: Arc::clone(&work),
                };
                let discipline = PooledDiscipline { desc };
                host_loop(
                    rx,
                    host,
                    HostLoop {
                        role: desc.role,
                        grants: GrantContract::Single(desc.grant),
                        sink: &sink,
                        // The pooled port is fire-and-forget (`Result<(), ()>`):
                        // there is no typed receipt to inform, so no mirror.
                        mirror: None,
                        discipline: &discipline,
                        fault: fault.as_deref(),
                    },
                );
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
/// The ONE host dispatch loop's fold map (ADR-079 D2) — everything
/// per-host is a field, exactly the [`HostPump`] field list the loop
/// builds. BOTH seat models compose [`host_loop`] with one of these: the
/// pooled `WorkQueue` pair (via [`PooledSink`] / [`PooledDiscipline`]) and
/// the keyed solve executor (its own `SolveSink` / `SolveDiscipline` over
/// `GrantContract::SolverPins`).
#[derive(Clone, Copy)]
pub(crate) struct HostLoop<'a> {
    /// The hosted role — the descriptor's pooled role or the Solver pin
    /// role; drives the queue len/cap keys and the posture consult.
    pub(crate) role: WorkerRole,
    /// The row-#6 grant-kind contract.
    pub(crate) grants: GrantContract,
    /// The seat model.
    pub(crate) sink: &'a dyn SeatSink,
    /// The typed-receipt advisory mirror (solve only; `None` = pooled).
    pub(crate) mirror: Option<&'a AtomicUsize>,
    /// The per-host abort wording owner.
    pub(crate) discipline: &'a dyn HostDiscipline,
    /// The S2 fault watch (`None` on hosts whose receipts are not
    /// pyo3-owned — they never enter Faulted).
    pub(crate) fault: Option<&'a crate::arb_engine::fleet_intake::IntakeFaultWatch>,
}
/// The ONE host dispatch loop (ADR-079 D2): build the unified [`HostPump`]
/// from the caller's [`HostLoop`] fold map and run the ONE
/// recv → apply → pump loop. (`rx` moves into [`HostPump::run`] — the
/// thread-boundary move the pre-fold loop needed a lint expectation for is
/// now `run`'s.)
pub(crate) fn host_loop(rx: mpsc::Receiver<HostMsg>, mut host: FleetHost, fold: HostLoop<'_>) {
    let HostLoop {
        role,
        grants,
        sink,
        mirror,
        discipline,
        fault,
    } = fold;
    let mut backlog = VecDeque::new();
    let mut no_progress = NoProgressGuard::new(intake_no_progress_ticks());
    HostPump {
        host: &mut host,
        backlog: &mut backlog,
        role,
        grants,
        sink,
        mirror,
        discipline,
        backstop: intake_backstop(),
        no_progress: &mut no_progress,
        fault,
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

#[cfg(test)]
mod loop_home_tests {
    /// The structural gate for ADR-079 D2: `seat_host` is the ONE home of
    /// the host dispatch loop. The keyed solve executor composes the
    /// shared loop behind its own seat model (`SolveSink`); a second
    /// `fn host_loop` definition anywhere in `arb_engine` is a fold
    /// violation — the loop vocabulary must not fork again.
    #[test]
    fn the_host_loop_is_defined_once_in_seat_host() {
        let sources: [(&str, &str); 5] = [
            ("seat_host/host.rs", include_str!("host.rs")),
            ("seat_host/pump.rs", include_str!("pump.rs")),
            (
                "fleet_solve_executor.rs",
                include_str!("../fleet_solve_executor.rs"),
            ),
            (
                "fleet_sim_executor.rs",
                include_str!("../fleet_sim_executor.rs"),
            ),
            (
                "fleet_registration_executor.rs",
                include_str!("../fleet_registration_executor.rs"),
            ),
        ];
        for (file, src) in sources {
            // A definition line is `fn host_loop` (after any visibility
            // qualifier) — doc mentions and this test's own needle stay out
            // of the count.
            let defs = src
                .lines()
                .filter(|line| {
                    let line = line.trim_start();
                    let line = line.strip_prefix("pub(crate) ").unwrap_or(line);
                    line.starts_with("fn host_loop")
                })
                .count();
            let want = usize::from(file == "seat_host/host.rs");
            assert_eq!(
                defs, want,
                "{file} must define {want} host_loop fn(s) — the ONE host dispatch loop lives in seat_host"
            );
        }
    }
}
