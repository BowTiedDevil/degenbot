//! The process-wide fleet boot statics (ADR-079 D1): `install_boot`, the
//! `global_executor` materializer, and the candidate-4
//! `BootSlot`/`FleetBootRegistry` keyed boot owner with the pooled roles'
//! descriptor rows — process-wide, not seat-scoped.

use crate::arb_engine::boot_stamp::{BootRole, BootStamp};
use crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor;
use crate::arb_engine::fleet_sim_executor::FleetSimExecutor;
use crate::arb_engine::seat_host::SeatRoleDesc;
use degenbot_core::op_error;
use degenbot_workers::budget::FleetBudget;
use degenbot_workers::dispatcher::{BootError, FleetBoot, GrantKind};
use degenbot_workers::role::WorkerRole;
use std::sync::OnceLock;

/// Install the CONSTRUCTION-STAMPED boot: the engine's own typed
/// boot descriptor (fleet quota + overrides + posture) parsed at ITS
/// construction from the CALLER cfg, stamped with the engine id + a
/// deterministic cfg hash. Never overrides an installed value (first
/// engine wins, like the other stance statics) — every construction after
/// the first RIDES, and the ride is ledgered (a divergent-cfg rider is
/// counted + warned in prod, ILLEGAL in tests) on the descriptor's
/// [`BootRole`] row.
pub(crate) fn install_boot(courier: &OnceLock<BootStamp>, desc: &SeatRoleDesc, stamp: BootStamp) {
    crate::arb_engine::boot_stamp::record_ride(desc.boot_role, &stamp);
    // FF-T5: record the resolved fleet profile ONCE (first
    // writer wins, like the stamp itself): the process summary feeds the
    // degenbot_fleet_profile metric, and a serial-tier resolution fires
    // the production alert (never a silent narrow).
    crate::arb_engine::fleet_status::record_fleet_profile_at_install(&stamp.boot());
    let _ = courier.set(stamp);
}
/// The process-wide materializer (the global_* boilerplate, folded from
/// the two executors): lazily boot the executor from the
/// construction-stamped boot and persist it for the process lifetime.
/// the absence window is CLOSED BY CONSTRUCTION — every dispatch
/// path builds on a constructed engine, and construction (`with_core_cfg`)
/// installs the stamp BEFORE any dispatch can exist. A missing stamp means
/// a caller skipped the construction contract: LOUD abort (never a silent
/// fallback boot of a boot nobody chose).
///
/// FF-T1: the BOOT-REFUSAL arm is TYPED and STICKY — never a
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
            reason = "the loud construction-contract abort IS the design: a stamp-less materialization must abort, never fall back silently"
        )]
        let stamp = courier.get().expect(desc.stamp_missing);
        boot(stamp.boot()).inspect_err(|err| {
            // FF-T1: the BOOT-REFUSAL family is typed and sticky,
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
// candidate 4: the FleetBootRegistry.
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
        "fleet sim boot stamp missing: an engine must construct before the first fleet submit",
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
        "fleet registration boot stamp missing: an engine must construct before the first fleet submit",
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
    /// FF-T1: the typed, sticky fleet boot refusal.
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
// ======================================================================
// Pins for the candidate-4 FleetBootRegistry contract.
// Written against the TARGET contract; production code is NOT changed.
// ======================================================================
#[cfg(test)]
mod candidate4_seam_pins {
    use super::{BootSlot, BootSlotView, FleetBootRegistry};
    use crate::arb_engine::boot_stamp::BootRole;
    use crate::arb_engine::fleet_registration_executor::FleetRegistrationExecutor;
    use crate::arb_engine::fleet_sim_executor::FleetSimExecutor;
    use crate::arb_engine::seat_host::SeatRoleDesc;
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
