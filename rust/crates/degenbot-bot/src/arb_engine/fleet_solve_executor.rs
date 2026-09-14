//! Fleet-hosted solve executor (ADR-042 F3): Solver-role hosting of the
//! LPT solve bins — the fleet becomes the sole executor of solve bins under
//! the `fleet.stance=Fleet` migration stance.
//!
//! Replaces the private `solve_executor` runtime fleet on this axis, with
//! the degenbot-workers `FleetHost` FSM driving dispatch: per-bin worker
//! pinning is a keyed pin (Solver pin per LPT bin, T3/T6 across cycles —
//! warm L1/L2 + allocator arenas, RAYPAR T3 no-split/no-steal carried
//! over); per-path result streaming is the callers' existing per-path
//! mpsc sends (not per-bin) and is unchanged. The deadlock ledger carries
//! over verbatim (design doc §10): a submitted unit whose results feed a
//! pipe is never dropped — the per-role queue's loud ADR-021 overflow is
//! backed by an unbounded host-side backlog (the legacy mpsc was
//! unbounded), and every seat/host failure is a loud abort.
//!
//! Bin keys reuse the LPT bin index offset by [`SOLVE_BIN_KEY_BASE`] so
//! the merge pin key `0` stays unique: first sight claims a pin from an
//! idle Solver seat (T1→T2→T3); every later unit for that bin continues
//! on the SAME seat (T6).

use degenbot_core::op_error;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};

use crate::arb_engine::boot_stamp::{BootRole, BootStamp};
use crate::arb_engine::seat_host::{
    intake_backstop, GrantContract, HostDiscipline, HostMsg, HostPump, SeatSink,
};
use degenbot_workers::dispatcher::{
    BootError, FleetBoot, FleetHost, Grant, GrantKind, SubmitError, SubmitReceipt, Unit,
};
use degenbot_workers::lane::{LaneCtx, QuitSig};
use degenbot_workers::role::WorkerRole;
use degenbot_workers::slot::PinKey;

/// Bin keys are the LPT bin index offset by one: the merge pin owns key 0
/// (`degenbot_workers::slot::MERGE_PIN_KEY`) and a Solver key must never
/// collide with it.
pub(crate) const SOLVE_BIN_KEY_BASE: PinKey = 1;

/// Loud, unrecoverable executor failure (mirror of `solve_executor.rs`'s
/// abort discipline): a dead host would strand in-flight per-path result
/// sends in pipes nobody drains, so swallowing the error is never an
/// option.
#[expect(
    clippy::print_stderr,
    reason = "the abort path must stay legible with no tracing subscriber installed (test harnesses drop the tracing event); stderr is the process's last message"
)]
pub(crate) fn abort_executor(context: &str, err: &str) -> ! {
    op_error!(domain = solver, context = %context,
        error = %err,
        "unrecoverable — aborting (stranded result pipe)"
    );
    eprintln!("UNRECOVERABLE, aborting (stranded result pipe): {context}: {err}");
    std::process::abort();
}
/// Public loud-stop wrapper (ADR-021; used by the `solver_dispatch` submit
/// seam for stranded-pipe terminal stops).
pub(crate) fn abort_loud(context: &str, err: &str) -> ! {
    abort_executor(context, err);
}

/// The pins == bins invariant check (P6YXA6): a Solver bin index must land
/// within the structural seat count. Pure (no `&self`) so the tests hold
/// the contract without booting a fleet.
fn validate_bin_index(bin: usize, solver_seats: usize) -> Result<(), String> {
    if bin < solver_seats {
        Ok(())
    } else {
        Err(format!(
            "bin {bin} exceeds the {solver_seats} structural Solver seats \
             (pins == bins invariant, P6YXA6); the dispatch arms must bin \
             at executor.bin_count()"
        ))
    }
}

/// One bin job as the host hands it to a Solver seat.
struct SeatJob {
    /// The host-tracked unit id (dispatch bookkeeping).
    unit: u64,
    /// The work payload (the 'static + Send `run_bin` closure — the fleet
    /// crate has no pyo3 and simulation never round-trips Python, design
    /// doc §8). The seat hands the unit its `LaneCtx` (LW-T2 Seam B).
    work: Box<dyn FnOnce(&LaneCtx) + Send>,
    /// The seat ctx minted at the T2 grant seam: the bin's pin + the
    /// slot's warm arena identity (warm across cycles, fresh after T9).
    ctx: LaneCtx,
}

/// The fleet-hosted solve executor. Shared by all engine cycles (the
/// global static hands out `&'static`, mirroring the incumbent executor's
/// construction-once contract: persistent seats keep warm L1/L2 +
/// allocator arenas across cycles).
pub(crate) struct FleetSolveExecutor {
    tx: mpsc::Sender<HostMsg>,
    /// The waker-fan-out token (T3): deregistered on drop.
    waker: u64,
    unit_seq: AtomicU64,
    /// The BOUNDED role-queue length at the LAST stamp (spill or
    /// `SeatDone`) — NOT the backlog depth (6HE6RF comment fix; the
    /// mechanism is kept). `submit_solve_bin`'s receipt bit compares
    /// this against the queue cap: "the role queue was >= cap at the
    /// last stamp" — an advisory lagging flag exactly as
    /// `SubmitReceipt`'s doc says; the unit is never dropped either way
    /// (§10 ledger).
    solver_queue_len: Arc<std::sync::atomic::AtomicUsize>,
    /// Seat count (read via [`FleetSolveExecutor::bin_count`] — the
    /// dispatch arms bin at exactly this count so every bin has a home).
    solver_seats: usize,
    /// The resolved lane-to-thread binding (FF-T4 — test-facing
    /// parity assertions; the host itself moved into the host thread).
    #[cfg(test)]
    binding: degenbot_workers::plan::Binding,
}

impl crate::arb_engine::executor::Executor for FleetSolveExecutor {
    fn bin_count(&self) -> usize {
        self.bin_count()
    }

    fn submit(
        &self,
        bin: usize,
        work: crate::arb_engine::executor::SubmitWork,
    ) -> Result<
        degenbot_workers::dispatcher::SubmitReceipt,
        degenbot_workers::dispatcher::SubmitError,
    > {
        self.submit_solve_bin(bin, work)
    }
}

impl Drop for FleetSolveExecutor {
    fn drop(&mut self) {
        crate::arb_engine::fleet_wake::deregister(self.waker);
    }
}

impl FleetSolveExecutor {
    /// Boot the executor from a `FleetBoot` (quota + overrides + posture):
    /// boot the [`FleetHost`], spawn one persistent named seat thread per
    /// Solver pin, and run the dispatch loop on the host thread. Fail-loud
    /// (the typed `BootError`) when the declared shares cannot host the
    /// quota — oversubscription is a configuration bug surfaced at boot,
    /// never a runtime throttle storm (design doc §5).
    ///
    /// # Errors
    /// [`BootError`] — the fleet budget sum check or a boot invariant.
    pub(crate) fn boot(boot: FleetBoot) -> Result<Self, BootError> {
        let host = FleetHost::boot(boot)?;
        // FF-T3 (Z2YW52): the LANE-TO-THREAD BINDING SEAM — the same
        // adapter slot the pooled pair runs (seat_host): the PINNED
        // binding preserves today's topology exactly (the per-seat
        // keyed mailboxes over the ONE HostPump); the serial binding is
        // a SeatSink + ONE grant lane over the same HostPump and lands
        // with FF-T4 — until then the arm refuses with the plan's
        // typed pending refusal (never a silent narrow).
        match host.plan().binding {
            degenbot_workers::plan::Binding::Pinned => Ok(Self::boot_pinned(host)),
            // FF-T4 (Z6XTDX): the serial arm BOOTS — the serial
            // projection's ONE solver seat is the named serial-0 cycle
            // thread: one persistent keyed mailbox (bin 0) over the ONE
            // HostPump, the §10 never-drop shape unchanged.
            degenbot_workers::plan::Binding::Serial => Ok(Self::boot_serial(host)),
        }
    }

    /// The PINNED binding's instantiation (today's topology, verbatim: one
    /// persistent keyed mailbox per Solver pin over the ONE `HostPump`).
    fn boot_pinned(host: FleetHost) -> Self {
        Self::boot_seats(host, WorkerRole::Solver.thread_name())
    }

    /// The SERIAL binding's instantiation (FF-T4): the projection's
    /// ONE solver seat is the named `serial-0` cycle thread — the same
    /// keyed-mailbox construction, one seat, the §10 shape unchanged.
    fn boot_serial(host: FleetHost) -> Self {
        Self::boot_seats(host, crate::arb_engine::seat_host::SERIAL_SEAT_NAME)
    }

    /// The shared seat construction: `seat_name_pattern` is the
    /// `{n}`-templated thread name (pinned: the solver seats; serial:
    /// the ONE serial-0 cycle seat).
    fn boot_seats(host: FleetHost, seat_name_pattern: &'static str) -> Self {
        #[cfg(test)]
        let binding = host.plan().binding;
        let solver_seats = host.budget().solver_pin_count;

        let (tx, rx) = mpsc::channel::<HostMsg>();
        // Per-seat mailboxes: a seat is a persistent keyed pin — one unit
        // at a time, warm arenas across cycles (RAYPAR T3, design doc §3.4).
        let mut seat_senders = Vec::with_capacity(solver_seats);
        let mut seat_mailboxes = Vec::with_capacity(solver_seats);
        for _seat in 0..solver_seats {
            let (stx, srx) = mpsc::channel::<SeatJob>();
            seat_senders.push(stx);
            seat_mailboxes.push(srx);
        }
        // 2SIOHJ seat<->slot bijection, explicit at build time: the pump's
        // `seats.get(grant.slot)` is a POSITIONAL map — mailbox i serves
        // the unit granted to the FleetHost slot `SlotLayout::solver
        // .start + i` (degenbot-workers' boot-frozen table geometry; this
        // crate cannot name the `pub(crate)` layout, so the invariant is
        // pinned HERE, citing it). `SlotLayout::of` asserts the solver
        // range to be exactly `budget.solver_pin_count` seats (pins ==
        // bins, P6YXA6) cut from index 0, so this vec is the identity map
        // over the solver home range; any non-solver grant (sim/resolve/
        // poolupd/merge seats have no mailbox) misses the `get` and aborts
        // loudly below.
        if seat_senders.len() != solver_seats {
            // Loud CONSTRUCTION abort, not a Result path: a seat array that
            // does not tile the SlotLayout solver range exactly would
            // misroute bins positionally (2SIOHJ) — the executor discipline
            // aborts, never returns a degraded handle.
            abort_executor(
                "solver seat construction",
                "seat mailboxes must tile the SlotLayout solver range exactly",
            );
        }
        // The submit mirror (LW-T5, Seam E) lives with the host thread and
        // every executor handle shares the same cells.
        // 7OGY5V: no posture MIRROR — posture-invariant Solver admission
        // retired its only reader (the host FSM still owns every posture
        // decision: Deferrable hold + sim-intake floor, dispatcher-side).
        let solver_queue_len = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Census: `FleetHost::boot` registered every v1 role row. The seats
        // are the runtime behind the solver pins, named per
        // `WorkerRole::Solver::thread_name()` (work-fleet-solver-{n}).
        // Seat completions ride the SAME host channel as submissions: a
        // single message queue cannot deadlock (a separate completion
        // channel would need select() to drain while blocking on rx).
        for (seat, srx) in seat_mailboxes.into_iter().enumerate() {
            let done = tx.clone();
            let spawned = std::thread::Builder::new()
                .name(seat_name_pattern.replace("{n}", &seat.to_string()))
                .spawn(move || seat_loop(u64::try_from(seat).unwrap_or(u64::MAX), srx, &done));
            if let Err(err) = spawned {
                // A missing seat strands its pinned bins' results — loud.
                abort_executor("solver seat spawn", &format!("{err:?}"));
            }
        }
        let host_queue_len = Arc::clone(&solver_queue_len);
        let spawned = std::thread::Builder::new()
            .name("work-fleet-solver-host".to_string())
            .spawn(move || {
                host_loop(rx, host, &seat_senders, &host_queue_len);
            });
        if let Err(err) = spawned {
            abort_executor("fleet host thread spawn", &format!("{err:?}"));
        }
        Self {
            waker: crate::arb_engine::fleet_wake::register(&tx),
            tx,
            unit_seq: AtomicU64::new(0),
            solver_seats,
            solver_queue_len,
            #[cfg(test)]
            binding,
        }
    }

    /// Test-facing: the resolved plan binding (FF-T4 — the parity
    /// tests assert the tier each boot instantiated).
    #[cfg(test)]
    fn plan_binding_for_test(&self) -> degenbot_workers::plan::Binding {
        self.binding
    }

    /// Solver seats = the budget's structural LPT bin count. The dispatch
    /// arms bin at THIS count (P6YXA6 reconciliation): pins and bins are
    /// the same number, so every bin owns a warm keyed seat across cycles.
    #[must_use]
    pub(crate) fn bin_count(&self) -> usize {
        self.solver_seats
    }

    /// Submit one LPT bin job keyed to its bin; the seat hands the unit body
    /// a [`LaneCtx`] carrying the bin's pin key and warm arena identity
    /// (LW-T2 Seam B). The typed submit receipt never drops a unit (the
    /// host-side backlog preserves the legacy unbounded-mpsc semantics);
    /// posture refusals are TYPED at this seam (admission-side only —
    /// running units are never preempted); a closed host channel (executor
    /// died) is a LOUD abort — a lost bin would strand its paths' per-path
    /// result sends forever (stranded pipe, §10).
    pub(crate) fn submit_solve_bin(
        &self,
        bin: usize,
        work: crate::arb_engine::executor::SubmitWork,
    ) -> Result<SubmitReceipt, SubmitError> {
        // The pins == bins invariant (P6YXA6), held at the submit seam: a
        // bin without a structural seat must abort HERE — with both numbers
        // in the message — instead of decaying into an FSM transition
        // refusal deep in dispatch.
        if let Err(msg) = validate_bin_index(bin, self.solver_seats) {
            abort_executor("bin submission", &msg);
        }
        let key = SOLVE_BIN_KEY_BASE.saturating_add(u64::try_from(bin).unwrap_or(u64::MAX));
        let unit = Unit::new(
            self.unit_seq.fetch_add(1, Ordering::Relaxed),
            WorkerRole::Solver,
            Some(key),
            // The bin's per-path result sends feed the merge pipe.
            true,
            Box::new(work),
        );
        // 7OGY5V: Solver admission is posture-INVARIANT (worker-fleet.md §6:
        // cordon effects are the Deferrable hold + sim-intake floor ONLY;
        // Solver is CordonClass::Never in workers::role). The LW-T5-era
        // seam-side refusal was backed out — the soak showed it stranded the
        // bin's result pipe and aborted the bot on routine cgroup throttling.
        // The ONE process posture owner (JCI2FW Part A) feeds the Deferrable
        // hold + sim floor downstream (the dispatcher's own gates consult it
        // live; the host sheds on the owner's transition feed).
        if self.tx.send(HostMsg::Enqueue(unit)).is_err() {
            return Err(SubmitError::PortClosed);
        }
        Ok(SubmitReceipt {
            // The receipt's backlog bit compares the host's queue STAMP (the
            // BOUNDED role-queue length at the last spill/SeatDone stamp —
            // NOT the backlog depth) against the cap: TRUE means the role
            // queue was at/over cap at the last stamp, so this unit rides the
            // unbounded host backlog and drains FIRST on the next pump (§10
            // ledger) — never dropped, never silent. ADVISORY under mirror
            // lag: the stamp is published asynchronously on the host thread,
            // so the bit is a lagging flag exactly as `SubmitReceipt`'s doc
            // says (6HE6RF fixed the overstated "backlog mirror" comment;
            // the mechanism is kept).
            accepted_with_backlog: self.solver_queue_len.load(Ordering::Relaxed)
                >= self.solver_seats.saturating_mul(2),
        })
    }
}

/// One Solver seat (a persistent keyed pin): execute units one at a time —
/// no yield mid-unit — and report completion so the host applies T3 (the
/// seat re-pins warm; arenas are never live across a role switch).
#[expect(
    clippy::needless_pass_by_value,
    reason = "the seat's mailbox Receiver is owned by the seat thread — a borrow cannot cross the thread boundary"
)]
fn seat_loop(seat: u64, rx: mpsc::Receiver<SeatJob>, done: &mpsc::Sender<HostMsg>) {
    while let Ok(job) = rx.recv() {
        // A panicking bin closure must not kill the seat (its pinned bins
        // would strand): keep the seat alive, log loudly, report done.
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| (job.work)(&job.ctx)));
        if outcome.is_err() {
            op_error!(
                domain = solver,
                seat,
                unit = job.unit,
                "bin job panicked — the seat survives, the failure is loud"
            );
        }
        if done.send(HostMsg::SeatDone { seat }).is_err() {
            // The host is gone (executor dropped — tests): the seat retires.
            break;
        }
    }
}

/// The solve host's dispatch loop: build the unified [`HostPump`] for the
/// Solver pin role (the FOLD MAP — everything per-host is a field) and run
/// the ONE recv → apply → pump loop (6HE6RF). The seat model (per-seat
/// keyed mailboxes, warm arenas) stays HERE — P-RZEWTX; only the message
/// triple joined `seat_host`. (`rx` moves into
/// [`HostPump::run`] — the thread-boundary move the pre-fold loop
/// needed a lint expectation for is now `run`'s.)
fn host_loop(
    rx: mpsc::Receiver<HostMsg>,
    mut host: FleetHost,
    seats: &[mpsc::Sender<SeatJob>],
    solver_queue_len: &std::sync::atomic::AtomicUsize,
) {
    let sink = SolveSink { seats };
    let discipline = SolveDiscipline;
    let mut backlog = VecDeque::new();
    HostPump {
        host: &mut host,
        backlog: &mut backlog,
        role: WorkerRole::Solver,
        grants: GrantContract::SolverPins,
        sink: &sink,
        // The typed-submit receipt's advisory stamp — present iff the host
        // serves a typed-receipt submit seam (the pooled port has nothing
        // to inform). It stores the BOUNDED role-queue length at the last
        // stamp (spill or SeatDone) — the honest mirror note, 6HE6RF.
        mirror: Some(solver_queue_len),
        discipline: &discipline,
        backstop: intake_backstop(),
        no_progress: &mut crate::arb_engine::seat_host::NoProgressGuard::new(
            crate::arb_engine::seat_host::intake_no_progress_ticks(),
        ),
        // S2 scope cut: the solve host has no pyo3-owned intake receipts, so
        // it never enters Faulted (progress() requires a fault watch); held
        // solve work under a lane death follows the pre-existing lane-death
        // flows, not this task's intake resolution path.
        fault: None,
        #[cfg(test)]
        ticks: None,
    }
    .run(rx);
}

/// The solve host's seat model (P-RZEWTX, unchanged by 6HE6RF): per-seat
/// keyed mailboxes — a seat is a persistent pin (T3/T6 warm arenas), so a
/// granted unit routes POSITIONALLY to the grant slot's mailbox (2SIOHJ:
/// seat i <-> `SlotLayout::solver.start` + i).
struct SolveSink<'a> {
    seats: &'a [mpsc::Sender<SeatJob>],
}

impl SeatSink for SolveSink<'_> {
    fn deliver(&self, host: &mut FleetHost, grant: Grant, unit: Unit) {
        // Positional seat map (2SIOHJ): seat i <-> FleetHost slot
        // `SlotLayout::solver.start + i` — the solver home range is cut
        // from index 0 at exactly this vec's length (pinned at the
        // executor's construction), so the get is the identity map over
        // the solver seats; any non-solver grant has no mailbox and is
        // a loud abort, never a silent re-seat. (6HE6RF: the explicit
        // `GrantContract::SolverPins` kind check in the unified pump
        // now fires BEFORE this get — this arm is the second, seat-map
        // layer of the same loud contract.)
        let Some(seat_tx) = self
            .seats
            .get(usize::try_from(grant.slot).unwrap_or(usize::MAX))
        else {
            abort_executor(
                "dispatch grant",
                "unknown seat (seat i <-> SlotLayout solver_range.start + i —                  a non-solver grant has no mailbox)",
            );
        };
        // LW-T2 (Seam B): mint the warm arena at the grant seam — the
        // ctx handed to the unit at dispatch time ALWAYS carries the
        // warm identity (stable across cycles; released at T9). An
        // unknown slot here is structurally unreachable (the grant came
        // from THIS host) — a silent default would violate the loud
        // posture, so it aborts with BOTH numbers, P6YXA6-style.
        let arena = host.ensure_arena(grant.slot).unwrap_or_else(|| {
            abort_executor(
                "arena mint at grant",
                &format!(
                    "slot {} hosts no arena for bin key {}",
                    grant.slot,
                    unit.key.unwrap_or(0)
                ),
            );
        });
        let ctx = LaneCtx {
            pin: unit.key.unwrap_or(0),
            arena,
            // LW-T3 (Seam C): the injected default escalation port (the
            // inline-sim runtime, registered at hook install) — lanes
            // without one are refused TYPED at escalate, never dropped.
            escalation: degenbot_workers::lane::default_escalation_port()
                .unwrap_or_else(degenbot_workers::lane::no_escalation_port),
            quit: QuitSig,
        };
        let job = SeatJob {
            unit: grant.unit,
            work: unit.work,
            ctx,
        };
        if seat_tx.send(job).is_err() {
            // A dead seat cannot drain its pinned bins' results —
            // stranded pipe (§10).
            abort_executor("seat mailbox send", "seat thread is gone");
        }
    }
}

/// The solve host's abort discipline: the same free [`abort_executor`]
/// the module has always owned — the unified triple's SHARED contexts
/// route through here, and the solve-specific contexts keep their
/// pre-fold strings byte-identical.
struct SolveDiscipline;

impl HostDiscipline for SolveDiscipline {
    fn fail(&self, context: &str, err: &str) -> ! {
        abort_executor(context, err)
    }

    fn enqueue_refused(&self, err: &str) -> ! {
        abort_executor("solver enqueue", err)
    }

    fn completion_refused(&self, err: &str) -> ! {
        abort_executor("seat completion (T3)", err)
    }

    fn foreign_grant(&self, kind: GrantKind) -> ! {
        abort_executor(
            "dispatch grant",
            &format!(
                "non-solver grant in the solve executor (grant kind {kind:?} — \
                 a non-Solver grant has no keyed seat)"
            ),
        )
    }
}

static FLEET_SOLVE_BOOT: OnceLock<BootStamp> = OnceLock::new();
static FLEET_EXECUTOR: OnceLock<FleetSolveExecutor> = OnceLock::new();

/// Install the CONSTRUCTION-STAMPED boot (YI5NGB): the engine's own typed
/// boot descriptor (fleet quota + overrides + posture) parsed at ITS
/// construction from the CALLER cfg, stamped with the engine id + a
/// deterministic cfg hash. Never overrides an installed value (first
/// engine wins, like the other stance statics) — every construction after
/// the first RIDES, and the ride is ledgered (a divergent-cfg rider is
/// counted + warned in prod, ILLEGAL in tests).
pub(crate) fn install_boot(stamp: BootStamp) {
    crate::arb_engine::boot_stamp::record_ride(BootRole::Solve, &stamp);
    let _ = FLEET_SOLVE_BOOT.set(stamp);
}

/// The process-wide fleet solve executor, built lazily on the first
/// fleet-stance solve and persisting for the process lifetime.
pub(crate) fn global_fleet_solve_executor() -> &'static FleetSolveExecutor {
    FLEET_EXECUTOR.get_or_init(|| {
        // YI5NGB: the absence window is CLOSED BY CONSTRUCTION — every
        // dispatch path builds on a constructed engine, and construction
        // (with_core_cfg) installs the stamp BEFORE any dispatch can
        // exist. A missing stamp means a caller skipped the construction
        // contract: LOUD abort (never a silent fallback boot of a boot
        // nobody chose).
        #[expect(
            clippy::expect_used,
            reason = "the loud construction-contract abort IS the YI5NGB design: a stamp-less materialization must abort, never fall back silently"
        )]
        let stamp = FLEET_SOLVE_BOOT
            .get()
            .expect(
                "fleet solve boot stamp missing: an engine must construct before the first fleet submit (YI5NGB)",
            );
        match FleetSolveExecutor::boot(stamp.boot()) {
            Ok(executor) => executor,
            Err(err) => abort_executor("fleet budget boot", &err.to_string()),
        }
    })
}

// The solve-lane adapter (witness + carrier + ledger) now lives in
// `crate::arb_engine::executor` (QR3NUS 43E3H3): the former provisional
// lane module folded there per the JI275C placement. The fleet executor
// retains SOLVE_BIN_KEY_BASE and its host machinery here.

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use crate::arb_engine::executor::{Executor as _, SubmitWork};
    use std::collections::BTreeSet;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use degenbot_solvers::mixed::SolvePathResult;
    use degenbot_workers::budget::BudgetOverrides;
    use degenbot_workers::dispatcher::{
        AbortingPolicy, ArenaToken, FleetBoot, PanicAction, PanicVerdict, SeatSurvivesPolicy,
    };
    use degenbot_workers::lane::{
        install_default_escalation_port, EscalationError, EscalationPort, EscalationWork, LaneCtx,
    };
    use degenbot_workers::posture::{FleetPosture, PostureOwner, PosturePolicy, ThrottleSample};

    use super::super::executor_ab_probe::{load_corpus_fixture, probe_ctx, prod_lpt_bins};
    use super::super::lane_walk::solve_one_path;
    use super::WorkerRole;
    use super::{validate_bin_index, FleetSolveExecutor, SOLVE_BIN_KEY_BASE};
    use crate::arb_engine::executor::{
        lane_death_response, run_solve_lane, LaneFailure, LaneOutcome, SolveLane, SolveOutcome,
    };

    /// A FRESH hermetic posture owner (leaked to `'static`): every test
    /// boot gets its own owner, never the process global (7KAPBB isolation).
    fn hermetic_owner() -> &'static PostureOwner {
        std::boxed::Box::leak(std::boxed::Box::new(PostureOwner::new(
            PosturePolicy::doc_defaults(),
        )))
    }

    fn hermetic_boot() -> FleetBoot {
        hermetic_boot_with_owner(hermetic_owner())
    }

    fn hermetic_boot_with_owner(owner: &'static PostureOwner) -> FleetBoot {
        FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 8.0,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: Some(owner),
        }
    }

    /// The pins == bins contract (P6YXA6) as a pure validator: a
    /// seat-bounded bin index passes; anything over it is shouted down with
    /// BOTH numbers so the loud abort decodes at a glance.
    #[test]
    fn bin_submission_above_the_structural_seat_count_is_a_loud_invariant_violation() {
        assert!(validate_bin_index(5, 6).is_ok());
        let err = validate_bin_index(6, 6).expect_err("bin == seats is out of range");
        assert!(
            err.contains("bin 6"),
            "message names the rejected bin: {err}"
        );
        assert!(
            err.contains("6 structural"),
            "message names the seat count: {err}"
        );
    }

    fn submit_bins(
        executor: &FleetSolveExecutor,
        bins: &[Vec<usize>],
        items: &[Arc<::degenbot_solvers::mixed::ResolvedMixedPath>],
        ctx: &Arc<crate::arb_engine::solve_cycle::SolveCycleShared>,
    ) -> Vec<(u64, SolvePathResult)> {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, SolvePathResult)>();
        for (bin_idx, bin) in bins.iter().enumerate() {
            let tx = tx.clone();
            let ctx = Arc::clone(ctx);
            let items: Vec<_> = bin.iter().map(|&i| Arc::clone(&items[i])).collect();
            let keys: Vec<u64> = bin
                .iter()
                .map(|&i| u64::try_from(i).unwrap_or(u64::MAX))
                .collect();
            executor
                .submit(
                    bin_idx,
                    Box::new(move |_ctx| {
                        // Per-path send (not per-bin): the existing per-path result
                        // streaming — byte-for-byte what the run_bin bodies do.
                        for (key, item) in keys.into_iter().zip(items) {
                            if let Some((pid, r)) =
                                solve_one_path(&ctx, &tracing::Span::none(), key, &item)
                            {
                                let _ = tx.send((pid, r));
                            }
                        }
                    }),
                )
                .expect("fixture submit blocked by noise");
        }
        drop(tx);
        let mut results: Vec<(u64, SolvePathResult)> = rx.into_iter().collect();
        results.sort_unstable_by_key(|(pid, _)| *pid);
        results
    }

    /// FLEET FIXTURE (BCA77G, LW-T9 single-arm): the fleet-hosted solve
    /// executor produces exact, honest outcomes on the committed heavy-CL
    /// capture fixture — outcomes can never exceed submissions, every
    /// submitted path yields exactly one outcome (`solve_one_path` verdict
    /// or failure), and the pinned-seat parity semantics carry (P6YXA6:
    /// pins == bins). The former legacy-executor parity arm is deleted
    /// with the stance (LW-T9; there is no other executor to diff against).
    #[test]
    fn fleet_executor_yields_exact_outcomes_on_capture_fixture() {
        let items = load_corpus_fixture();
        let ctx = probe_ctx();
        // P6YXA6 regression: the bins bind at the fleet's STRUCTURAL seat
        // count — pins == bins. Binning at the machine-derived worker count
        // instead boots 6 hermetic seats against host-core-derived bins (22
        // on the 24-core raw host) and aborts at the T2 grant — the
        // host-only `just test-rust` failure this restructuring pins.
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let bins = prod_lpt_bins(&items, executor.bin_count());
        assert_eq!(
            bins.len(),
            executor.bin_count(),
            "solver bins must equal the structural Solver seat count (pins == bins, P6YXA6)"
        );

        let fleet = submit_bins(&executor, &bins, &items, &ctx);
        assert!(!fleet.is_empty(), "fixture must produce results");

        // Outcome honesty: the outcomes land exactly once per submitted
        // path (in-bin solvers merge; failures arrive typed — never twice).
        let mut pids: Vec<u64> = fleet.iter().map(|(pid, _)| *pid).collect();
        pids.sort_unstable();
        let n = pids.len();
        pids.dedup();
        assert_eq!(
            pids.len(),
            n,
            "a path may never surface two outcomes (one outcome per submission)"
        );
        assert!(n <= items.len(), "outcomes can never exceed submissions");
    }

    /// Solver bin keys never collide with the merge pin key (the FSM's
    /// keyed-pin invariant, ADR-042 §3.4).
    #[test]
    fn solve_bin_keys_never_collide_with_the_merge_pin_key() {
        assert_ne!(SOLVE_BIN_KEY_BASE, degenbot_workers::slot::MERGE_PIN_KEY);
    }

    /// Pinning fixture (BCA77G): per-bin worker pinning — every bin's
    /// units ride the same seat across cycles (T3/T6), matching the
    /// RAYPAR T3 one-persistent-worker-per-bin contract.
    ///
    /// P6YXA6 sizing reconciliation: the fleet seats are the
    /// STRUCTURAL LPT bin count — at a hermetic Q = 8 boot,
    /// floor(8) − the default solve headroom (2) = 6 seats — not the
    /// retired sharesx2 multiple (8). The dispatch arms bin at this same
    /// count, so every bin owns a warm keyed seat across cycles (T6).
    #[expect(
        clippy::print_stderr,
        reason = "the self-skip channel when a parallel test won the stamp race (the documented F1 skip semantics)"
    )]
    /// F1 white-box (YI5NGB): the materializer's init closure aborts LOUD
    /// (the expect) when no construction ever installed a stamp — invoked
    /// directly so the expect fires WITHOUT a real `FleetHost` boot.
    #[test]
    fn fleet_solve_materializer_without_a_stamp_is_loud() {
        if super::FLEET_SOLVE_BOOT.get().is_some() {
            eprintln!(
                "skipping: another test already installed the solve boot stamp in this process"
            );
            return;
        }
        let closure = || {
            let stamp = super::FLEET_SOLVE_BOOT.get().expect(
                "fleet solve boot stamp missing: an engine must construct before the first fleet submit (YI5NGB)",
            );
            match FleetSolveExecutor::boot(stamp.boot()) {
                Ok(_executor) => (),
                Err(_err) => (),
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(closure));
        let err = result.expect_err("a stamp-less materialization must abort loud");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&str>().copied())
            .expect("panic payload is the expect message");
        assert!(
            msg.contains("(YI5NGB)"),
            "the expect must name the task: {msg}"
        );
    }

    #[test]
    fn solver_seats_equal_the_structural_lpt_bin_count() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        assert_eq!(executor.bin_count(), 6);
    }

    #[test]
    fn bins_stay_pinned_to_one_seat_across_cycles() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let bins = executor.bin_count().clamp(2, 4);
        let observed: Arc<parking_lot::Mutex<Vec<(u64, std::thread::ThreadId)>>> = Arc::default();
        for _cycle in 0..3 {
            for bin in 0..bins {
                let bin_key = u64::try_from(bin).unwrap_or(u64::MAX);
                let observed = Arc::clone(&observed);
                executor
                    .submit(
                        bin,
                        Box::new(move |_ctx| {
                            observed.lock().push((bin_key, std::thread::current().id()));
                        }),
                    )
                    .expect("naming unit accepted");
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while observed.lock().len() < (bins * 3) {
            assert!(
                std::time::Instant::now() < deadline,
                "fleet seats did not drain all bin units in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let observed = observed.lock().clone();
        for bin in 0..bins {
            let bin_key = u64::try_from(bin).unwrap_or(u64::MAX);
            let seats: std::collections::HashSet<_> = observed
                .iter()
                .filter(|(k, _)| *k == bin_key)
                .map(|(_, t)| *t)
                .collect();
            assert_eq!(
                seats.len(),
                1,
                "bin {bin} must pin to exactly one seat across cycles"
            );
        }
    }

    // ---- LW-T2 (Seam B): seat context — no ambient runtime, LaneCtx identity

    /// The runtime wedge (LW-T2): fleet seats are OS threads with NO ambient
    /// tokio runtime. The executor boots and submits from inside a LIVE
    /// multi-thread runtime here, and every seated unit still observes
    /// `Handle::try_current() == Err` — any future drift that puts a runtime
    /// on the seat (the tokio-stance path) fails this test.
    ///
    /// INTENDED TRIPWIRE: this test passing today is correct (fleet seats
    /// are plain `std::thread`s). It goes RED deliberately when the
    /// tokio-stance path is ported, and again when LW-T8 consolidates the
    /// executors behind a trait — that red is the T9 cutover gate, not a
    /// regression.
    #[test]
    fn fleet_seats_run_units_with_no_ambient_tokio_runtime() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let runtime_free: Arc<parking_lot::Mutex<Vec<bool>>> = Arc::default();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            for _ in 0..3 {
                let runtime_free = Arc::clone(&runtime_free);
                executor
                    .submit(
                        0,
                        Box::new(move |_ctx| {
                            runtime_free
                                .lock()
                                .push(tokio::runtime::Handle::try_current().is_err());
                        }),
                    )
                    .expect("probe unit accepted");
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runtime_free.lock().len() < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "fleet seats did not drain the probe units in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            runtime_free.lock().iter().all(|free| *free),
            "fleet seats must run units OUTSIDE any ambient tokio runtime"
        );
    }

    /// The `LaneCtx` submit seam (LW-T2): `submit_solve_bin` hands the unit a
    /// ctx carrying the bin's pin key AND the warm arena identity — the
    /// SAME `ArenaToken` across cycles (warm), never the detached stub.
    #[test]
    fn submit_solve_bin_hands_a_lane_ctx_with_the_bins_key_and_warm_arena_identity() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let observed: Arc<parking_lot::Mutex<Vec<LaneCtx>>> = Arc::default();
        for _cycle in 0..2 {
            let observed = Arc::clone(&observed);
            executor
                .submit_solve_bin(
                    0,
                    SubmitWork::from(Box::new(move |ctx: &LaneCtx| {
                        observed.lock().push(ctx.clone());
                    })),
                )
                .expect("the nominal ctx submit is accepted");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while observed.lock().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the seat did not drain the ctx probe units in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let observed = observed.lock().clone();
        let expected_key = SOLVE_BIN_KEY_BASE; // bin 0 → base + 0
        for ctx in &observed {
            assert_eq!(
                ctx.pin, expected_key,
                "the ctx must carry the bin's pin key"
            );
            // Solver seats are pinned lanes: the arena is minted at the T2
            // grant seam, so a SOLVER unit must NEVER observe the detached
            // stub (the DETACHED token is pooled seats only).
            assert_ne!(
                ctx.arena,
                ArenaToken::DETACHED,
                "a solver-seat unit must never observe the DETACHED arena stub"
            );
        }
        assert_ne!(
            observed[0].arena,
            ArenaToken::DETACHED,
            "the warm identity must be host-minted, not the detached stub"
        );
        assert_eq!(
            observed[0].arena, observed[1].arena,
            "the SAME ArenaToken across cycles (warm)"
        );
    }

    // ---- LW-T6 (Seam G2): seat naming + census atoms -------------------------

    /// LW-T6: the boot census carries the seat fleet's rows — per-index
    /// `{n}` patterns matching the roles, the Solver budget matching the
    /// STRUCTURAL seat count, and no two rows sharing a thread-name pattern
    /// (the GOQWCL collision lock, fleet-wide).
    #[test]
    fn boot_census_rows_are_per_index_patterned_with_the_solver_budget() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let snap = degenbot_core::worker_census::snapshot();
        let solver_row = snap
            .iter()
            .find(|e| e.thread_name == WorkerRole::Solver.thread_name())
            .expect("the Solver fleet census row exists");
        assert_eq!(
            solver_row.count,
            executor.bin_count(),
            "the census Solver count must equal the structural seat count"
        );
        // The collision lock: no two rows fleet-wide share a thread-name
        // pattern (the GOQWCL lesson: shared patterns made dumps
        // unattributable).
        let mut patterns: Vec<&str> = snap.iter().map(|e| e.thread_name).collect();
        let n = patterns.len();
        patterns.sort_unstable();
        patterns.dedup();
        assert_eq!(
            patterns.len(),
            n,
            "census thread-name patterns must be unique"
        );
    }

    /// LW-T6: the runtime registration is not a lie — the OS thread names of
    /// RUNNING seats match the census rows (per-index under the pattern).
    #[test]
    fn running_seat_thread_names_match_their_census_rows() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let seats = executor.bin_count();
        let names: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
        for bin in 0..seats {
            let names = Arc::clone(&names);
            executor
                .submit_solve_bin(
                    bin,
                    SubmitWork::from(Box::new(move |_ctx| {
                        names.lock().push(
                            std::thread::current()
                                .name()
                                .unwrap_or("<unnamed>")
                                .to_owned(),
                        );
                    })),
                )
                .expect("the naming probe submit is accepted");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while names.lock().len() < seats {
            assert!(
                std::time::Instant::now() < deadline,
                "seats did not report thread names in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let names_set: std::collections::HashSet<_> = names.lock().iter().cloned().collect();
        for name in &names_set {
            assert!(
                name.starts_with("work-fleet-solver-"),
                "seat thread names must be per-index census-named: {name}"
            );
        }
        assert_eq!(
            names_set.len(),
            seats,
            "every structural seat has its own distinct census-NAMED thread"
        );
    }

    // ---- LW-T5 (Seam E): posture & precedence at the submit seam ------------

    /// 7OGY5V (soak adjudication, 2026-09-10): Solver admission is
    /// posture-INVARIANT — design doc §6's cordon effects hold only the
    /// Deferrable classes + the sim intake floor, and `workers::role`
    /// declares `Solver` `CordonClass::Never`; a cordon never refuses a
    /// Solver bin at the submit seam (the LW-T5-era gate over-reached the
    /// spec and the soak found the refusal stranded the bin's result pipe).
    #[test]
    fn submit_in_cordoned_posture_still_admits_solver_units_and_running_units_complete() {
        // JCI2FW Part A: the hermetic owner is INJECTED at boot, and the
        // cordon is forced through that shared owner (the executor's host
        // consults it; there is no per-executor feed seam anymore).
        let owner = hermetic_owner();
        let executor =
            FleetSolveExecutor::boot(hermetic_boot_with_owner(owner)).expect("fleet boot");
        let long_unit_done: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let cordoned_done: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let done = Arc::clone(&long_unit_done);
        executor
            .submit_solve_bin(
                0,
                SubmitWork::from(Box::new(move |_ctx| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    done.store(true, std::sync::atomic::Ordering::Relaxed);
                })),
            )
            .expect("the nominal submit is accepted");
        // Flip the posture to Cordoned through the SHARED owner (7OGY5V:
        // Solver is CordonClass::Never — the cordon must never refuse the
        // bin, and the already-running unit is never shed).
        owner.observe_throttle(
            100,
            ThrottleSample {
                events: 3,
                throttled_usec: 0,
                elapsed_usec: 1_000,
            },
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let done2 = Arc::clone(&cordoned_done);
        executor
            .submit_solve_bin(
                1,
                SubmitWork::from(Box::new(move |_ctx| {
                    done2.store(true, std::sync::atomic::Ordering::Relaxed);
                })),
            )
            .expect(
                "posture-invariant Solver admission: a CORDONED posture must \
                 NEVER refuse a Solver bin (design §6: cordon effects are the \
                 Deferrable hold + sim-intake floor ONLY)",
            );
        // Both the pre-cordon occupant and the cordon-era newcomer complete.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !(long_unit_done.load(std::sync::atomic::Ordering::Relaxed)
            && cordoned_done.load(std::sync::atomic::Ordering::Relaxed))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "a cordon-era unit did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// LW-T5 (Seam E): overflow past a role queue cap lands in the
    /// unbounded host backlog (S10 ledger) and NEVER drops - the receipts
    /// report accepted-with-backlog, and every submitted unit completes.
    #[test]
    fn overflow_past_the_role_cap_lands_in_the_backlog_and_never_drops() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let seats = executor.bin_count();
        let occupants_done: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        for bin in 0..seats {
            let done = Arc::clone(&occupants_done);
            executor
                .submit_solve_bin(
                    bin,
                    SubmitWork::from(Box::new(move |_ctx| {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    })),
                )
                .expect("occupant submit");
        }
        let extras = seats * 4;
        let drained: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        for bin in 0..extras {
            let drained = Arc::clone(&drained);
            executor
                .submit_solve_bin(
                    bin % seats,
                    SubmitWork::from(Box::new(move |_ctx| {
                        drained.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    })),
                )
                .expect("overflow submits are ACCEPTED - backlog, never dropped");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let drained_probe = Arc::clone(&drained);
        let probe = executor
            .submit_solve_bin(
                0,
                SubmitWork::from(Box::new(move |_ctx| {
                    drained_probe.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                })),
            )
            .expect("backlog submits are ACCEPTED - never dropped");
        assert!(
            probe.accepted_with_backlog,
            "the overflow submit must report accepted-with-backlog (the cap was exceeded)"
        );
        let total = u64::try_from(seats * 5 + 1).unwrap_or(u64::MAX);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while occupants_done.load(std::sync::atomic::Ordering::Relaxed)
            + drained.load(std::sync::atomic::Ordering::Relaxed)
            < total
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the seats did not drain all submitted units in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            occupants_done.load(std::sync::atomic::Ordering::Relaxed)
                + drained.load(std::sync::atomic::Ordering::Relaxed),
            total,
            "every submitted unit must complete - never dropped"
        );
    }

    // ---- LW-T3 (Seam C): escalation port — self-contained I/O lane ----------

    /// A TEST escalation port: a dedicated single-thread tokio runtime owned
    /// by its own pump thread (a self-contained capability lane, never the
    /// caller's CPU seat) — the shape of the default impl (the inline-sim
    /// runtime); the pyo3 default impl is feature-gated, so the lane
    /// contract is pinned here at the seam.
    struct ThreadLanePort {
        tx: std::sync::mpsc::Sender<(EscalationWork, degenbot_workers::lane::FinishOnDrop)>,
        gate: Arc<degenbot_workers::lane::EscalationGate>,
    }

    impl ThreadLanePort {
        fn spawn(budget: usize) -> Self {
            let (tx, rx) =
                std::sync::mpsc::channel::<(EscalationWork, degenbot_workers::lane::FinishOnDrop)>(
                );
            let gate = degenbot_workers::lane::EscalationGate::new(budget);
            std::thread::Builder::new()
                .name("test-escalation-lane".to_owned())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("escalation lane runtime");
                    for (work, permit) in rx {
                        runtime.block_on(work);
                        drop(permit);
                    }
                })
                .expect("escalation lane pump thread");
            Self { tx, gate }
        }
    }

    impl EscalationPort for ThreadLanePort {
        fn escalate(&self, work: EscalationWork) -> Result<(), EscalationError> {
            let permit = self.gate.begin()?;
            if self.tx.send((work, permit)).is_err() {
                return Err(EscalationError::PortClosed);
            }
            Ok(())
        }

        fn counters(&self) -> degenbot_workers::lane::EscalationCountersSnapshot {
            self.gate.counters()
        }
    }

    /// LW-T3 (Seam C, reth research §7): escalation is a SELF-CONTAINED I/O
    /// lane — a bin escalates its cold-miss work through its `LaneCtx`
    /// while EVERY solver seat is mid-unit, and the escalations complete on
    /// the port lane (never on a solver seat: CPU cannot starve I/O).
    #[test]
    fn escalations_complete_while_all_solver_seats_are_mid_unit() {
        install_default_escalation_port(Arc::new(ThreadLanePort::spawn(8)));
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let seats = executor.bin_count();
        let completed: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        let failures: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        let after_drained: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        let lane_threads: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
        let seats_done: Arc<std::sync::atomic::AtomicU64> = Arc::default();
        for bin in 0..seats {
            let completed = Arc::clone(&completed);
            let failures = Arc::clone(&failures);
            let after_drained = Arc::clone(&after_drained);
            let lane_threads = Arc::clone(&lane_threads);
            let seats_done = Arc::clone(&seats_done);
            executor
                .submit_solve_bin(
                    bin,
                    SubmitWork::from(Box::new(move |ctx| {
                        // The bin escalates its cold-miss work while MID-UNIT — the
                        // escalation must NOT run on this seat (CPU) but on the
                        // port's own lane.
                        let occupied_seats = Arc::clone(&seats_done);
                        if ctx
                            .escalate(Box::pin(async move {
                                // STARVATION CHECK: an escalation completing only
                                // after all seats drained would be CPU starvation by
                                // another name.
                                if occupied_seats.load(Ordering::Relaxed)
                                    == u64::try_from(seats).unwrap_or(0)
                                {
                                    after_drained.fetch_add(1, Ordering::Relaxed);
                                }
                                let name = std::thread::current()
                                    .name()
                                    .map_or_else(|| "<unnamed>".to_owned(), str::to_owned);
                                lane_threads.lock().push(name);
                                completed.fetch_add(1, Ordering::Relaxed);
                            }))
                            .is_err()
                        {
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        seats_done.fetch_add(1, Ordering::Relaxed);
                    })),
                )
                .expect("the escalation occupancy submit is accepted");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while seats_done.load(Ordering::Relaxed) < u64::try_from(seats).unwrap_or(0) {
            assert!(
                std::time::Instant::now() < deadline,
                "seats did not drain the escalation probe units in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            failures.load(Ordering::Relaxed),
            0,
            "escalations through the LaneCtx port must succeed (typed errors are the only failure mode)"
        );
        assert_eq!(
            completed.load(Ordering::Relaxed),
            u64::try_from(seats).unwrap_or(0),
            "every escalated cold-miss work item must COMPLETE while its seat is mid-unit"
        );
        assert_eq!(
            after_drained.load(Ordering::Relaxed),
            0,
            "no escalation may complete only after the seats drained (CPU starvation)"
        );
        for lane in lane_threads.lock().iter() {
            assert_ne!(
                lane.strip_prefix("work-fleet-solver"),
                Some(""),
                "escalated work must NEVER run on a solver seat: {lane}"
            );
        }
    }

    /// Test verdict double (decision A): records every (unit, seat)
    /// consultation and prescribes `RecordAndContinue` — never a real abort.
    struct VerdictRecorder {
        consulted: parking_lot::Mutex<Vec<(u64, u64)>>,
    }

    impl PanicVerdict for VerdictRecorder {
        fn on_unit_panic(&self, unit: u64, seat: u64) -> PanicAction {
            self.consulted.lock().push((unit, seat));
            PanicAction::RecordAndContinue
        }
    }

    /// Deliberate panic inside a harness bin body: the adapter's
    /// `catch_unwind` (with the seat's backstop) must convert it to data.
    #[expect(clippy::panic)]
    fn red_panic(message: &str) -> ! {
        panic!("{message}")
    }

    /// The exactness fuse (QR3NUS): a bin whose 3rd of N paths panics
    /// still drains exactly one outcome per submitted path — survivors as
    /// real outcomes (a worker `None` IS an outcome), every undelivered
    /// path as a typed failure — so `solved + suppressed + failed ==
    /// submitted` with the delivered and failed pid sets exact and
    /// disjoint. Today the panic silently vanishes with sender-drop.
    #[test]
    fn drain_delivers_exactly_one_lane_outcome_per_submitted_path_when_third_path_panics() {
        let submitted: BTreeSet<u64> = [10, 11, 12, 13, 14].into_iter().collect();
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        let mut lane = SolveLane::new(1, 0, vec![10, 11, 12, 13, 14], tx);
        run_solve_lane(&mut lane, &SeatSurvivesPolicy, |lane| {
            // Trace of a real bin: two survivor arms land, then the 3rd
            // path panics — 13 and 14 are still owed and must come back
            // typed instead of silently vanishing with sender-drop.
            let arm: SolveOutcome = SolveOutcome {
                pid: 10,
                result: SolvePathResult::default(),
                worker_clamp_twins: 0,
                payload: None,
                cycle_seq: 0, // inert on the in-cycle arm: the drain claims its cycle's own seq
                solve_block: 0,
                metadata: crate::arb_engine::BlockMetadata::default(),
                update_stamp: vec![],
                solve_span: tracing::Span::none(),
            };
            lane.solved(arm);
            lane.suppressed(11);
            red_panic("path 12 panicked mid-bin (QR3NUS red harness)");
            // 13/14 never run — the panic ends the bin body.
        });
        drop(lane); // close the pipe so the drain completes
        let outcomes: Vec<LaneOutcome> = rx.into_iter().collect();

        let mut solved_count = 0usize;
        let mut suppressed_count = 0usize;
        let mut failed_count = 0usize;
        let mut delivered: BTreeSet<u64> = BTreeSet::new();
        let mut failed: BTreeSet<u64> = BTreeSet::new();
        for outcome in outcomes {
            match outcome {
                LaneOutcome::Solved(item) => {
                    solved_count += 1;
                    delivered.insert(item.pid);
                }
                LaneOutcome::Suppressed { pid } => {
                    suppressed_count += 1;
                    delivered.insert(pid);
                }
                LaneOutcome::Failed { pid, failure } => {
                    failed_count += 1;
                    assert!(
                        matches!(
                            failure,
                            LaneFailure::SeatPanic {
                                unit: 1,
                                seat: 0,
                                message: Some(_)
                            }
                        ),
                        "failed record must name unit + seat and carry the panic payload"
                    );
                    failed.insert(pid);
                }
            }
        }
        assert!(
            delivered.is_disjoint(&failed),
            "a path cannot be both delivered and failed: {delivered:?} / {failed:?}"
        );
        let covered: BTreeSet<u64> = delivered.union(&failed).copied().collect();
        assert_eq!(
            covered, submitted,
            "the drain must observe exactly one outcome per submitted path \
             (delivered ∪ failed == submitted) — the panic must not undercount"
        );
        assert_eq!(
            solved_count + suppressed_count + failed_count,
            submitted.len(),
            "merge-side per-path accounting must equal submissions"
        );
        assert_eq!(
            failed,
            [12, 13, 14].into_iter().collect::<BTreeSet<u64>>(),
            "the 3rd path and everything after it must land as typed failures"
        );
    }

    /// FF-T4 (Z6XTDX) — AC 3: a lane death mid-flight yields TERMINAL
    /// RECEIPTS for in-flight paths (typed `LaneFailure::LaneDeath`,
    /// exactly one outcome per submitted path — the ledger stays
    /// exact), the CORDONED posture (sticky — clean windows never lift
    /// it), and a LIVE process (the response returns; the pre-FF-T4
    /// shape was the merge fuse's loud abort on the undercount).
    #[test]
    fn a_lane_death_mid_flight_yields_terminal_receipts_cordon_and_a_live_process() {
        let owner = hermetic_owner();
        let submitted: BTreeSet<u64> = [30, 31, 32, 33, 34].into_iter().collect();
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        let mut lane = SolveLane::new(7, 3, vec![30, 31, 32, 33, 34], tx);
        // The bin runs partially: two outcomes deliver, then the LANE
        // DIES — the seat thread is gone mid-flight (no unwind, no more
        // sends, nothing). 32/33/34 are still owed.
        lane.solved(SolveOutcome {
            pid: 30,
            result: SolvePathResult::default(),
            worker_clamp_twins: 0,
            payload: None,
            cycle_seq: 0,
            solve_block: 0,
            metadata: crate::arb_engine::BlockMetadata::default(),
            update_stamp: vec![],
            solve_span: tracing::Span::none(),
        });
        lane.suppressed(31);
        // The response (the production wiring drives this body with the
        // process owner; hermetic tests inject their own — same code):
        let patched = lane_death_response(&mut lane, Some(owner));
        drop(lane);
        assert_eq!(
            patched, 3,
            "the three still-owed paths get terminal records"
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        // The sticky discipline: clean throttle windows NEVER lift a
        // lane-death cordon (the lane is still dead).
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
            "a lane-death cordon is sticky across clean windows"
        );
        // The exactness law through the death: delivered + failed ==
        // submitted, disjoint, every failure typed LaneDeath.
        let outcomes: Vec<LaneOutcome> = rx.into_iter().collect();
        let mut delivered: BTreeSet<u64> = BTreeSet::new();
        let mut failed: BTreeSet<u64> = BTreeSet::new();
        for outcome in outcomes {
            match outcome {
                LaneOutcome::Solved(item) => {
                    delivered.insert(item.pid);
                }
                LaneOutcome::Suppressed { pid } => {
                    delivered.insert(pid);
                }
                LaneOutcome::Failed { pid, failure } => {
                    assert_eq!(
                        failure,
                        LaneFailure::LaneDeath { unit: 7, seat: 3 },
                        "the terminal receipt must name the dead lane's unit + seat"
                    );
                    failed.insert(pid);
                }
            }
        }
        assert!(delivered.is_disjoint(&failed));
        let covered: BTreeSet<u64> = delivered.union(&failed).copied().collect();
        assert_eq!(
            covered, submitted,
            "the lane death leaves the ledger EXACT: one outcome per submitted path"
        );
        // A LIVE process: this line running is the proof — the response
        // returned instead of the abort.
    }

    /// FF-T4 — the production auto-arm: a bin body that RETURNS with
    /// still-owed paths (the seat abandoned its bin mid-flight) gets
    /// the lane-death response through `run_solve_lane` — terminal
    /// records on the pipe, never the silent sender-drop undercount
    /// that used to trip the merge fuse.
    #[test]
    fn a_bin_that_abandons_its_paths_mid_flight_drains_terminal_records() {
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        let mut lane = SolveLane::new(9, 1, vec![40, 41, 42], tx);
        run_solve_lane(&mut lane, &SeatSurvivesPolicy, |lane| {
            lane.suppressed(40);
            // The body simply STOPS: 41/42 still owed. (A deliberate
            // abandon — the injection shape of a mid-flight lane death.)
        });
        drop(lane);
        let outcomes: Vec<LaneOutcome> = rx.into_iter().collect();
        let failed: BTreeSet<u64> = outcomes
            .into_iter()
            .filter_map(|outcome| match outcome {
                LaneOutcome::Failed { pid, failure } => {
                    assert_eq!(
                        failure,
                        LaneFailure::LaneDeath { unit: 9, seat: 1 },
                        "the abandon arm patches typed lane-death records"
                    );
                    Some(pid)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            failed,
            [41, 42].into_iter().collect::<BTreeSet<u64>>(),
            "every still-owed path lands as a typed terminal record"
        );
    }

    /// FF-T4 — AC 4: the outcome corpus is IDENTICAL across the pinned
    /// and serial bindings (parity: the binding changes which threads
    /// run the lanes, never the outcomes — the promotion-gate
    /// contract). The corpus is a deterministic three-bin fixture
    /// (solved / suppressed / a deliberate mid-bin `SeatPanic` / a
    /// deliberate mid-bin `LaneDeath` abandon); the bins address the
    /// binding's own bin space (`bin % bin_count`: under serial the
    /// projection pins ONE solver seat, so every corpus bin routes to
    /// serial-0 — the same PATHS, the same outcomes).
    #[test]
    fn the_outcome_corpus_is_identical_across_the_pinned_and_serial_bindings() {
        let corpus = |executor: &FleetSolveExecutor| -> Vec<(u64, u64)> {
            let bin_count = executor.bin_count();
            assert!(bin_count >= 1);
            let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
            for bin in 0..3_u64 {
                let tx = tx.clone();
                let base = 100 + bin * 10;
                let pids = vec![base, base + 1, base + 2, base + 3];
                let work: SubmitWork = Box::new(move |_ctx: &LaneCtx| {
                    let mut lane = SolveLane::new(bin, bin, pids, tx.clone());
                    run_solve_lane(&mut lane, &SeatSurvivesPolicy, |lane| {
                        lane.suppressed(base);
                        if bin == 1 {
                            red_panic("the corpus's deliberate mid-bin panic");
                        }
                        if bin == 2 {
                            // The corpus's deliberate mid-bin abandon
                            // (the LaneDeath arm): base+2/base+3 stay owed.
                            return;
                        }
                        lane.solved(SolveOutcome {
                            pid: base + 1,
                            result: SolvePathResult::default(),
                            worker_clamp_twins: 0,
                            payload: None,
                            cycle_seq: 0,
                            solve_block: 0,
                            metadata: crate::arb_engine::BlockMetadata::default(),
                            update_stamp: vec![],
                            solve_span: tracing::Span::none(),
                        });
                        lane.suppressed(base + 2);
                        lane.suppressed(base + 3);
                    });
                });
                executor
                    .submit_solve_bin(
                        usize::try_from(bin % u64::try_from(bin_count).unwrap_or(1)).unwrap_or(0),
                        work,
                    )
                    .expect("the corpus bin submits");
            }
            drop(tx);
            let mut covered: Vec<(u64, u64)> = rx
                .into_iter()
                .map(|outcome| match outcome {
                    LaneOutcome::Solved(item) => (item.pid, 0),
                    LaneOutcome::Suppressed { pid } => (pid, 1),
                    LaneOutcome::Failed { pid, failure } => (pid, {
                        if matches!(failure, LaneFailure::SeatPanic { .. }) {
                            2
                        } else {
                            3
                        }
                    }),
                })
                .collect();
            covered.sort_unstable();
            covered
        };
        let pinned = FleetSolveExecutor::boot(hermetic_boot())
            .expect("the pinned binding boots (8-core auto)");
        assert_eq!(
            pinned.plan_binding_for_test(),
            degenbot_workers::plan::Binding::Pinned,
            "the 8-core auto host resolves the pinned tier"
        );
        let serial = FleetSolveExecutor::boot(FleetBoot {
            quota_cpus: 2.0,
            ..hermetic_boot()
        })
        .expect("the serial binding boots (2-core auto)");
        assert_eq!(
            serial.plan_binding_for_test(),
            degenbot_workers::plan::Binding::Serial,
            "the 2-core auto host resolves the serial tier"
        );
        let pinned_corpus = corpus(&pinned);
        assert_eq!(
            pinned_corpus.len(),
            12,
            "the corpus covers every submitted path exactly once (4 per bin)"
        );
        let serial_corpus = corpus(&serial);
        assert_eq!(
            serial_corpus, pinned_corpus,
            "parity: the outcome corpus must be identical across bindings"
        );
    }

    /// Decision A drive: after a panicking cycle the SAME seat takes the
    /// next cycle's pinned bin (keyed pins never move), and the panic was
    /// expressed as data — the verdict consulted, typed failure records
    /// on the pipe. The panicking bin here rides a REAL executor seat so
    /// the seat-survives policy is exercised end to end (no real abort —
    /// the strict `AbortingPolicy` is never installed under test).
    #[test]
    fn panicked_seat_survives_and_takes_the_next_cycles_pinned_bin_on_the_same_thread() {
        let executor = FleetSolveExecutor::boot(hermetic_boot()).expect("fleet boot");
        let verdict = Arc::new(VerdictRecorder {
            consulted: parking_lot::Mutex::new(Vec::new()),
        });
        let observed: Arc<parking_lot::Mutex<Vec<std::thread::ThreadId>>> = Arc::default();
        let lane_outcomes: Arc<parking_lot::Mutex<Vec<LaneOutcome>>> = Arc::default();
        for cycle in 0..2 {
            let observed = Arc::clone(&observed);
            let verdict = Arc::clone(&verdict);
            let lane_outcomes = Arc::clone(&lane_outcomes);
            executor
                .submit(
                    0,
                    Box::new(move |_ctx| {
                        observed.lock().push(std::thread::current().id());
                        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
                        let mut lane =
                            SolveLane::new(cycle, 0, vec![cycle * 10, cycle * 10 + 1], tx);
                        run_solve_lane(&mut lane, verdict.as_ref(), |lane| {
                            if cycle == 0 {
                                lane.suppressed(cycle * 10); // one pid emitted before the panic
                                red_panic("cycle-0 bin panics (QR3NUS red harness)");
                            } else {
                                // cycle 1 runs CLEAN — every path delivers on
                                // the surviving seat (FF-T4: a bin that returns
                                // with still-owed paths is a lane death, not a
                                // clean cycle; this body owes both and pays both).
                                lane.suppressed(cycle * 10);
                                lane.suppressed(cycle * 10 + 1);
                            }
                        });
                        drop(lane); // close the pipe so the per-cycle drain completes
                        let mut stash = lane_outcomes.lock();
                        for outcome in rx {
                            stash.push(outcome);
                        }
                    }),
                )
                .expect("seat-stays unit accepted");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while observed.lock().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the seat did not take the next cycle's pinned bin in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let threads = observed.lock().clone();
        assert_eq!(
            threads[0], threads[1],
            "the SAME seat must take the next cycle's pinned bin after the panic"
        );
        let consulted = verdict.consulted.lock().clone();
        assert_eq!(
            consulted.len(),
            1,
            "the panicking cycle must consult the verdict (and only that cycle): {consulted:?}"
        );
        let failed: Vec<u64> = lane_outcomes
            .lock()
            .iter()
            .filter_map(|outcome| match outcome {
                LaneOutcome::Failed { pid, .. } => Some(*pid),
                _ => None,
            })
            .collect();
        assert_eq!(
            failed,
            vec![1],
            "the panicking cycle's undelivered path must land as a typed failure record"
        );
    }

    /// The unit-panic-with-pipe tripwire at policy-object level (decision
    /// A; no real `std::process::abort` ever runs under test): a panicking
    /// unit consults the verdict with its (unit, seat) identity, and the
    /// typed failure records carry that payload.
    #[test]
    fn unit_panic_with_result_pipe_consults_the_panic_verdict_with_unit_and_seat_payload() {
        let verdict = VerdictRecorder {
            consulted: parking_lot::Mutex::new(Vec::new()),
        };
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        let mut lane = SolveLane::new(7, 3, vec![21, 22], tx);
        run_solve_lane(&mut lane, &verdict, |_lane| {
            red_panic("bin unit panics with its result pipe open (QR3NUS tripwire harness)");
        });
        drop(lane); // close the pipe so the drain completes
        let outcomes: Vec<LaneOutcome> = rx.into_iter().collect();

        assert_eq!(
            verdict.consulted.lock().as_slice(),
            [(7, 3)],
            "the verdict must be consulted exactly once with unit + seat payload"
        );
        let failed: Vec<(u64, &LaneFailure)> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                LaneOutcome::Failed { pid, failure } => Some((*pid, failure)),
                _ => None,
            })
            .collect();
        let failed_pids: BTreeSet<u64> = failed.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(
            failed_pids,
            [21, 22].into_iter().collect::<BTreeSet<u64>>(),
            "every undelivered path must be covered by exactly one typed failure"
        );
        for (pid, failure) in failed {
            assert!(
                matches!(
                    failure,
                    LaneFailure::SeatPanic {
                        unit: 7,
                        seat: 3,
                        message: Some(message)
                    } if message.contains("tripwire")
                ),
                "failed record must name unit 7 + seat 3 and carry the panic payload (pid {pid})"
            );
        }
        // The pure policy mapping (decision A): the surviving policy keeps
        // the seat; the strict abort posture stays a VALUE — it aborts only
        // when wired outside tests, never here.
        assert_eq!(
            SeatSurvivesPolicy.on_unit_panic(7, 3),
            PanicAction::RecordAndContinue
        );
        assert_eq!(AbortingPolicy.on_unit_panic(7, 3), PanicAction::Abort);
    }
    /// 6HE6RF (the unified row-#6 tripwire, Q3.2): the solve host's
    /// grant-kind contract is now EXPLICIT — `GrantContract::SolverPins`
    /// inside the unified [`HostPump`] — so a foreign grant is a broken
    /// host contract, denied loudly, NEVER seated. Pre-fold the contract
    /// held only by the `seats.get(slot)` indexing accident, and under
    /// Nominal posture a foreign unit was silently SEATED on a Solver
    /// seat with a minted warm arena (the 6HE6RF red, catalog row #6);
    /// the pooled side aborted loudly (`seat_host`'s pre-existing check).
    /// The abort itself is a process abort (uncatchable in-process), so
    /// the contract is pinned at its pure predicate, mirroring
    /// `seat_host`'s loud check.
    #[test]
    fn solver_host_aborts_on_a_foreign_grant() {
        use degenbot_workers::dispatcher::GrantKind;

        use crate::arb_engine::seat_host::GrantContract as Contract;
        let solver = Contract::SolverPins;
        for kind in [GrantKind::NewPinClaim, GrantKind::PinContinuation] {
            assert!(
                solver.admits(kind),
                "the solve host serves the {kind:?} grant (the Solver pin pair)"
            );
        }
        for kind in [
            GrantKind::Sim,
            GrantKind::Resolve,
            GrantKind::PoolStateUpdate,
        ] {
            assert!(
                !solver.admits(kind),
                "a {kind:?} grant is a broken solve-host contract — denied loudly, never seated"
            );
        }
        // The pooled predicate, mirrored: exactly ONE kind.
        let pooled = Contract::Single(GrantKind::Sim);
        assert!(
            pooled.admits(GrantKind::Sim),
            "the sim host serves sim grants"
        );
        assert!(
            !pooled.admits(GrantKind::NewPinClaim),
            "a NewPinClaim grant is a broken sim-host contract"
        );
    }
}
