//! `fleet_intake` — the PRG-3 port: the crate's ONLY public surface into the
//! pooled fleet intake. The port (`FleetIntake`) is the pub NAME; the
//! executors implementing it stay crate-internal (zero exported types).
//! No pyo3 in any signature (`executor.rs` doc rule); the unit body stays
//! `Box<dyn FnOnce() + Send + 'static>` — the `Python::attach` rides
//! degenbot-python's closure body, never a port item. HARD RATCHET: the
//! surface is this port + (after commit 2) 2 pub fns — add nothing.
//!
//! FF-T1 AMENDMENT: the two hand-outs went fallible — a refused
//! fleet boot surfaces the typed, sticky workers `BootError` instead of
//! aborting the host process (the pyo3 leaf maps it onto the
//! `BootRefused` exception). No new fn, no new type: the SAME two
//! hand-outs, one honest `Result` arm (the library never aborts on the
//! boot-refusal arm).
use degenbot_workers::dispatcher::BootError;
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::time::Duration;
/// The typed terminal record for a Faulted intake (TB4QGX T6, spike S2):
/// the host drained held work because the lane-death latch is sticky, so no
/// later admit can ever respect it. `held` is the number of queued/backlogged
/// units resolved by the drain — they ran ZERO times (resolution != execution),
/// so at-most-once holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeFault {
    /// The typed cause (today always `"lane-death"`).
    pub cause: &'static str,
    /// The held units resolved by the fault (never executed).
    pub held: usize,
}
/// The S2 fault watch: the bot-side seam the pyo3 receipt observes. The host
/// `set`s it on entering Faulted (first-wins — a second lane death for the
/// same latch is idempotent); every waiting receipt resolves terminally
/// instead of parking. This is the ONLY cross-crate surface the S2 seam adds
/// (the `FleetIntake` port itself is unchanged).
#[derive(Debug, Default)]
pub struct IntakeFaultWatch {
    state: Mutex<Option<IntakeFault>>,
    cv: Condvar,
}
impl IntakeFaultWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// The current fault, if any (read-through).
    #[must_use]
    pub fn snapshot(&self) -> Option<IntakeFault> {
        *self.state.lock()
    }
    /// First-wins: only the first fault is stored; later calls are no-ops
    /// (idempotent under double lane-death delivery).
    pub fn set(&self, fault: IntakeFault) {
        let mut state = self.state.lock();
        if state.is_none() {
            *state = Some(fault);
            self.cv.notify_all();
        }
    }
    /// Block until a fault lands (or `timeout` elapses); returns it.
    #[must_use]
    pub fn wait_for(&self, timeout: Duration) -> Option<IntakeFault> {
        let mut state = self.state.lock();
        if state.is_none() {
            self.cv.wait_for(&mut state, timeout);
        }
        *state
    }
}
/// The installed registration executor's S2 fault watch — what the pyo3
/// receipt observes. `None` before any engine construction (a submit would
/// already have refused with the typed `BootError`). Deliberately NOT a
/// separate process-global: the watch belongs to the executor, so hermetic
/// executors isolate completely (7KAPBB).
#[must_use]
pub fn registration_fault_watch() -> Option<Arc<IntakeFaultWatch>> {
    crate::arb_engine::seat_host::FleetBootRegistry::process()
        .registration()
        .global_executor(
            crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::boot,
        )
        .ok()
        .map(crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::fault_watch)
}
/// The pooled work unit: the seat threads' existing box shape, pinned as
/// an alias (concrete, object-safe — never a generic on the port).
pub type InnerWork = Box<dyn FnOnce() + Send + 'static>;
/// The port: fire-and-dispatch a pooled unit; no response from the
/// executor (the unit self-reports through its own closure channel).
pub trait FleetIntake: Send + Sync {
    /// Submit one pooled unit; slots are the budget's per-role cap and
    /// never overflow the executor (host-side backlog spill is fail-loud).
    /// The unit body carries its own completion channel (per-request
    /// receipt semantics). Lost-key (no pin affinity): the seat picks
    /// the unit up as soon as a slot frees.
    fn spawn(&self, work: InnerWork);
}
/// The sim-side sibling (one line: the fleet sim executor upcast); consumed
/// by `executor.rs::global_sim_executor()` so the §3.1 re-route reads
/// through ONE module; crate-internal because only the sim dispatch route
/// needs it.
/// YI5NGB construction precondition: the fleet materializes LAZILY on the
/// first call — from the construction-STAMPED boot the first engine
/// construction installed (`with_core_cfg`). No fallback exists: a caller
/// that reaches this seam BEFORE any engine construction aborts LOUD.
///
/// # Errors
/// FF-T1: the typed, sticky fleet boot refusal — the sticky
/// `BootError` the materializer parked (never a process abort).
pub(crate) fn sim_intake() -> Result<&'static dyn FleetIntake, BootError> {
    crate::arb_engine::seat_host::FleetBootRegistry::process()
        .sim()
        .global_executor(crate::arb_engine::fleet_sim_executor::FleetSimExecutor::boot)
        .map(|exec| exec as &'static dyn FleetIntake)
}
/// The ONLY surface `degenbot-python` names: the pooled registration intake.
/// The PRG-3 station ratchet + the `DivergenceTable` note from sim (one seam,
/// two shapes) + the `ADR-013`/pyo3-free boundary: units carry `InnerWork`;
/// `Python::attach` stays in `degenbot-python`'s closure body.
/// YI5NGB construction precondition (same as [`sim_intake`]): the fleet
/// materializes LAZILY from the first construction's STAMPED boot — the
/// PRG-5 probe's pre-engine `False` / post-engine `True` latch rides this
/// exact install moment. No fallback exists: a caller that reaches this
/// seam BEFORE any engine construction aborts LOUD.
///
/// # Errors
/// FF-T1: the typed, sticky fleet boot refusal — surfaced at
/// the submit seam BEFORE any unit is enqueued (never a process abort;
/// the pyo3 leaf maps it onto the `BootRefused` exception).
pub fn registration_intake() -> Result<&'static dyn FleetIntake, BootError> {
    crate::arb_engine::seat_host::FleetBootRegistry::process()
        .registration()
        .global_executor(
            crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::boot,
        )
        .map(|exec| exec as &'static dyn FleetIntake)
}
/// Whether an engine installed the fleet registration boot descriptor (the
/// PRG-5 gate read). A `bool` is fully public - the wrap stays legal across
/// the commit-2 module-private flip.
#[must_use]
pub fn registration_boot_installed() -> bool {
    // candidate 4: the PRG-5 gate reads the registry's first-wins
    // process latch, so it stays byte-equivalent with
    // `fleet_status::fleet_runtime_status().fleet_booted`.
    crate::arb_engine::seat_host::FleetBootRegistry::process().boot_installed()
}
#[cfg(test)]
// The loud-expect fixture style mirrors the executor fixture modules; the
// module-level expect is the documented-permitted form.
#[expect(clippy::expect_used)]
mod tests {
    use super::{FleetIntake, InnerWork};
    use degenbot_workers::budget::BudgetOverrides;
    use degenbot_workers::dispatcher::FleetBoot;
    use degenbot_workers::posture::{PostureOwner, PosturePolicy};
    use std::sync::{Arc, Mutex};
    // Copied from fleet_registration_executor.rs — the module's existing
    // fixture kit, module-local (no new helpers; the design's fixture note).
    // JCI2FW Part A: a fresh hermetic posture owner per boot — never the
    // process global (7KAPBB isolation).
    fn hermetic_boot() -> FleetBoot {
        FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 8.0,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: Some(std::boxed::Box::leak(std::boxed::Box::new(
                PostureOwner::new(PosturePolicy::doc_defaults()),
            ))),
        }
    }
    /// T3 `pooled_spawn_failure_modes_stay_pinned`: two compile-level pins.
    /// (i) An exhaustive `match` over the private `try_send`'s
    /// `Result<(), ()>` — re-widening the in-crate close modeling breaks
    /// compile until this match is updated deliberately. (ii) The
    /// object-safety let-binding — a generic parameter leaking into
    /// `FleetIntake::spawn` makes the port NOT dyn-compatible and this
    /// binding (hence the whole facade return type) stops compiling.
    #[test]
    fn pooled_spawn_failure_modes_stay_pinned() {
        let executor =
            crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::boot(
                hermetic_boot(),
            )
            .expect("fleet intake boot");
        // The object-safety pin: the binding compiles only while the port
        // stays dyn-compatible (the concrete alias parameter, never a
        // generic).
        let port: &dyn FleetIntake = &executor;
        let (tx, rx) = std::sync::mpsc::channel::<u64>();
        port.spawn(Box::new(move || {
            let _ = tx.send(1);
        }));
        let got = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert_eq!(got, Ok(1), "the port delivered one unit");
        // The failure-vocabulary pin: the in-crate close modeling is exactly
        // `Result<(), ()>` — exhaustive over BOTH arms, no third shape.
        let sent: InnerWork = Box::new(|| {});
        match executor.try_send(sent) {
            Ok(()) => {}
            Err(()) => unreachable!("a live executor's host channel is open"),
        }
    }
    /// Copied from `fleet_registration_executor.rs` - the module's existing
    /// fixture kit, module-local (no new helpers; the design's fixture note).
    fn await_receipts<T: Send + 'static>(
        rx: &std::sync::mpsc::Receiver<T>,
        want: usize,
        deadline: std::time::Instant,
    ) -> Vec<T> {
        let mut got: Vec<T> = Vec::new();
        while got.len() < want {
            assert!(
                std::time::Instant::now() < deadline,
                "fleet intake seats did not drain {want} units in time (got {})",
                got.len()
            );
            if let Ok(v) = rx.try_recv() {
                got.push(v);
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        got
    }
    /// T1 `fleet_intake_facade_is_just_the_delegate`: boot a PRIVATE
    /// hermetic reg executor, take the exact upcast shape the facade fn body
    /// uses, submit 8 units through the port, assert exactly 8 receipts AND
    /// that a closure mutating shared state LANDS (the Box upcast did not
    /// eat the unit). Pins the R2 upcast chain end-to-end (concrete executor
    /// -> anonymous vtable -> fn call).
    #[test]
    fn fleet_intake_facade_is_just_the_delegate() {
        use std::sync::mpsc;
        let executor =
            crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::boot(
                hermetic_boot(),
            )
            .expect("fleet intake boot");
        let intake: &dyn FleetIntake = &executor;
        let shared = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (tx, rx) = mpsc::channel::<u8>();
        for id in 0..8_u8 {
            let tx = tx.clone();
            let shared = Arc::clone(&shared);
            intake.spawn(Box::new(move || {
                shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(id);
                let _ = tx.send(id);
            }));
        }
        drop(tx);
        let got = await_receipts(
            &rx,
            8,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        assert_eq!(got.len(), 8, "exactly 8 receipts through the port");
        let landed = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            landed.len(),
            8,
            "every unit's body ran - the facade must not wrap, copy, or eat state"
        );
    }
    /// The never-drop flood, through the FACADE ADAPTER the pyo3 leaf holds
    /// (`&dyn FleetIntake`). Boots a PRIVATE hermetic executor - never the
    /// process global: the global lazy materialization is CONSTRUCTION-keyed
    /// (it consumes the with_core_cfg-stamped boot; the ambient-deriving
    /// absence hatch is deleted since YI5NGB) and UPSERTS the
    /// `fleet_pool_state_updater_slots` census row (process-wide storage,
    /// `worker_census.rs` `CENSUS` table - an exactness hazard for the
    /// census-row tests under parallel test threads). The flood scale rides
    /// the hermetic executor's own `seat_count()` (the reg:511 fixture's
    /// shape, widened on
    /// the padding term), keeping the fixture machine-INDEPENDENT.
    #[test]
    fn fleet_intake_facade_preserves_the_never_drop_flood() {
        let executor =
            crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor::boot(
                hermetic_boot(),
            )
            .expect("fleet intake boot");
        let intake: &dyn crate::arb_engine::fleet_intake::FleetIntake = &executor;
        let cap = executor.seat_count();
        let (tx, rx) = std::sync::mpsc::channel::<u64>();
        // A flood far past the seat pool AND the 2x queue bound (the :511
        // fixture's discipline, widened on the padding term).
        for id in 0..(4 * cap + 64) as u64 {
            let tx = tx.clone();
            intake.spawn(Box::new(move || {
                let _ = tx.send(id);
            }));
        }
        drop(tx);
        let want = (4 * cap + 64) as u64;
        let got = await_receipts(
            &rx,
            usize::try_from(want).unwrap_or(usize::MAX),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        assert_eq!(
            got.len() as u64,
            want,
            "the facade adapter must not eat work"
        );
    }
}
