//! Fleet-hosted inline-sim executor (ADR-042 F4): `SimDriver`-role hosting of
//! the pipelined inline sims — the fleet becomes the sole executor of sim
//! units under the `fleet.stance=Fleet` migration stance. The incumbent
//! per-path `arb-sim-{pid}` detached-thread spawn (a burst of ~85–105
//! mostly-parked threads per cycle) retires onto a warm pooled seat set;
//! the legacy stance keeps the incumbent runtime byte-for-byte.
//!
//! The seat pool is the budget's `SimDriver` slot cap (design doc §5 —
//! today's `SimSlots` cap, `fleet.sim_slot_cap` terminal override), and the
//! host FSM's dispatch lane 2 gives queued sims precedence over new Solver
//! intake while cordoning floors sim intake (in-flight sims are never
//! cancelled — the `SimSlots` release-on-drop discipline carried over).
//!
//! Pacing note (the one deliberate difference): the incumbent semaphore
//! acquired the slot on the detached thread BEFORE the sim body ran and
//! released it exactly when the sim finished; the fleet replaces that pair
//! with the host's pooled-seat bound — a seat IS the granted slot, and the
//! grant lane caps concurrent executing sims at the budget slot cap. The
//! scheduling bin never blocks (the legacy parked-before-exec threads were
//! the unbounded part of the burst); submission over the never-drop
//! host channel is the pacing seam now.
//!
//! RZEWTX: the pooled-seat machinery (`WorkQueue`, `seat_loop`,
//! `host_loop`, `apply_host_msg`/`pump` admission, the boot install/global
//! boilerplate) is SHARED with the registration executor — ONE seat host
//! (`arb_engine::seat_host`) parameterized by the `FleetBootRegistry`'s
//! `SIM_ROLE` descriptor (candidate 4 moved the descriptor row there; this
//! module owns only the executor + boot fn). 6HE6RF: the solve executor's host-MESSAGE triple joins
//! that machinery too (the ONE [`HostPump`] behind all three fleet
//! hosts); its SEAT MODEL (per-seat keyed mailboxes, warm arenas) and
//! typed submit seam stay in `fleet_solve_executor.rs` — the RZEWTX
//! design gate now covers only the seat models (see `seat_host`'s
//! module doc).
//!
//! Sim units carry no pin key (the `SimDriver` role is pooled, T5: run →
//! back-to-idle); the merge pin / Solver-pin lanes of the shared host FSM
//! never fire here because this executor only enqueues `SimDriver` units.
//! The `WrapDatabaseAsync` runtime-capture caveat (ADR-042 §8) is
//! unchanged: the sim body still enters via the installed hook, whose
//! task-spawn executes on a multi-thread runtime worker.
//!
//! GIL ruling (task LTUE7I): the hook body is verified Python-free —
//! `degenbot_python::simulation::inline_hook` imports no `pyo3` symbol
//! and never attaches the GIL on its hot path, so hosting the closure on
//! fleet seats crosses the FFI only at the existing install/delivery
//! seams (design doc §8: simulation never round-trips Python).

use degenbot_workers::dispatcher::{BootError, FleetBoot};

use crate::arb_engine::seat_host::{self, SeatHost};

/// The fleet-hosted inline-sim executor. Shared by all engine cycles (the
/// registry's global slot hands out `&'static`, mirroring the fleet solve
/// executor's construction-once contract: warm pooled seats across cycles).
pub(crate) struct FleetSimExecutor {
    /// The shared pooled-seat host (the channel submit end + the unit
    /// sequence).
    host: SeatHost,
    /// Test-facing seat count (the budget's `SimDriver` slot cap).
    #[cfg(test)]
    sim_seats: usize,
}

impl FleetSimExecutor {
    /// Boot from a `FleetBoot` (quota + overrides + posture): boot the
    /// [`FleetHost`], spawn the pooled `SimDriver` seat threads (the
    /// budget's sim slot cap, `work-fleet-sim-{n}` census naming), and
    /// run the dispatch loop on the host thread. Fail-loud (the typed
    /// [`BootError`]) when the declared shares cannot host the quota.
    ///
    /// candidate 4 (YUMQU3): the seat descriptor is read from the
    /// `FleetBootRegistry` slot — this module owns no role static. Sim hosts
    /// carry NO intake fault watch (`None`): their receipts are not
    /// pyo3-owned (S2 scope cut), so they never enter Faulted.
    ///
    /// # Errors
    /// [`BootError`] — the fleet budget sum check or a boot invariant.
    pub(crate) fn boot(boot: FleetBoot) -> Result<Self, BootError> {
        let desc = crate::arb_engine::seat_host::FleetBootRegistry::process()
            .sim()
            .descriptor();
        let host = SeatHost::boot(desc, boot, None)?;
        Ok(Self {
            #[cfg(test)]
            sim_seats: host.seat_count(),
            host,
        })
    }

    /// The budget's `SimDriver` slot cap (the pooled seat count). Test-facing
    /// (the scheduling sites submit without asking the cap).
    #[cfg(test)]
    pub(crate) fn sim_slot_cap(&self) -> usize {
        self.sim_seats
    }

    /// Test-facing: the resolved plan binding (FF-T4 — the tier the boot
    /// instantiated for this host).
    #[cfg(test)]
    pub(crate) fn host_plan_binding(&self) -> degenbot_workers::plan::Binding {
        self.host.plan_binding()
    }
}

seat_host::impl_seat_hosted!(FleetSimExecutor, host);

#[cfg(test)]
// The panic-survival fixture panics deliberately (loud-assert test style;
// the module-level expect is the documented-permitted form).
#[expect(clippy::expect_used, clippy::panic)]
mod tests {
    #[expect(
        clippy::print_stderr,
        reason = "the self-skip channel when a parallel test won the stamp race (the documented F1 skip semantics)"
    )]
    /// F1 white-box (YI5NGB): the materializer's init closure aborts LOUD
    /// (the expect) when no construction ever installed a stamp — invoked
    /// directly so the expect fires WITHOUT a real `FleetHost` boot.
    #[test]
    fn fleet_sim_materializer_without_a_stamp_is_loud() {
        let slot = crate::arb_engine::seat_host::FleetBootRegistry::process().sim();
        if slot.boot_installed() {
            eprintln!(
                "skipping: another test already installed the sim boot stamp in this process"
            );
            return;
        }
        let closure = || {
            let _ = slot.global_executor(FleetSimExecutor::boot);
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

    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use degenbot_workers::budget::BudgetOverrides;
    use degenbot_workers::dispatcher::FleetBoot;
    use degenbot_workers::posture::{FleetPosture, PostureOwner, PosturePolicy, ThrottleSample};

    use super::FleetSimExecutor;

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

    /// Wait for a minimum receipt count without unbounded blocking
    /// (deadline-poll, the solve-parity fixture style).
    fn await_receipts<T: Send + 'static>(
        rx: &mpsc::Receiver<T>,
        want: usize,
        deadline: Instant,
    ) -> Vec<T> {
        let mut got: Vec<T> = Vec::new();
        while got.len() < want {
            assert!(
                Instant::now() < deadline,
                "fleet sim seats did not drain {want} units in time (got {})",
                got.len()
            );
            if let Ok(v) = rx.try_recv() {
                got.push(v);
                continue;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        got
    }

    /// The fleet's `SimDriver` seats must run each submitted sim to
    /// completion, every receipt delivered exactly once.
    #[test]
    fn every_submitted_sim_completes_exactly_once() {
        let executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        let (tx, rx) = mpsc::channel::<u64>();
        for id in 0..32_u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let mut got = await_receipts(&rx, 32, Instant::now() + Duration::from_secs(10));
        got.sort_unstable();
        let want: Vec<u64> = (0..32).collect();
        assert_eq!(got, want, "every sim unit completes exactly once");
    }

    /// Seats are the fleet `SimDriver` role: census thread-name pattern
    /// work-fleet-sim-{n} (GOQWCL rule — greppable, never the shared
    /// tokio-runtime-worker default).
    #[test]
    fn sims_execute_on_named_fleet_simdriver_seats() {
        let executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        let (tx, rx) = mpsc::channel::<String>();
        for _ in 0..8 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(
                    std::thread::current()
                        .name()
                        .map(str::to_owned)
                        .unwrap_or_default(),
                );
            });
        }
        drop(tx);
        let names = await_receipts(&rx, 8, Instant::now() + Duration::from_secs(10));
        for name in names {
            assert!(
                name.starts_with("work-fleet-sim-"),
                "sim unit must execute on a fleet SimDriver seat, got {name:?}"
            );
        }
    }

    /// A panicking sim must not kill the seat pool: the seat survives, the
    /// failure is logged loudly, and later sims drain normally.
    #[test]
    fn a_panicking_sim_unit_does_not_kill_the_seat_pool() {
        let executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        let (tx, rx) = mpsc::channel::<&'static str>();
        let tx_bomb = tx.clone();
        executor.spawn(move || {
            let _ = tx_bomb.send("boom");
            panic!("deliberate sim body panic (fixture)");
        });
        for _ in 0..4 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send("ok");
            });
        }
        drop(tx);
        // The bomb's pre-panic marker plus the four healthy receipts.
        let got = await_receipts(&rx, 5, Instant::now() + Duration::from_secs(10));
        assert_eq!(got.iter().filter(|s| **s == "boom").count(), 1);
        assert_eq!(got.iter().filter(|s| **s == "ok").count(), 4);
    }

    /// The seat pool is the budget's `SimDriver` slot cap (design doc §5
    /// `SimDriver` slots = today's `SimSlots` cap; `fleet.sim_slot_cap` is the
    /// terminal override).
    #[test]
    fn sim_seat_pool_is_the_budget_sim_slot_cap() {
        let executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        assert_eq!(
            executor.sim_slot_cap(),
            degenbot_workers::budget::DEFAULT_SIM_SLOT_CAP
        );
    }

    /// The pacing contract the incumbent `SimSlots` semaphore provided: no
    /// more sims execute CONCURRENTLY than the budget's sim slot cap —
    /// the fleet's pooled seats now provide that bound by construction.
    #[test]
    fn concurrent_sims_are_bounded_by_the_sim_slot_cap() {
        let executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        let cap = executor.sim_slot_cap();
        let inflight = Arc::new(parking_lot::Mutex::new(0_usize));
        let max_seen = Arc::new(parking_lot::Mutex::new(0_usize));
        let (tx, rx) = mpsc::channel::<()>();
        for _ in 0..(cap * 3) {
            let tx = tx.clone();
            let inflight = Arc::clone(&inflight);
            let max_seen = Arc::clone(&max_seen);
            executor.spawn(move || {
                let seen = *max_seen.lock();
                *inflight.lock() += 1;
                let cur = *inflight.lock();
                if cur > seen {
                    *max_seen.lock() = cur;
                }
                std::thread::sleep(Duration::from_millis(5));
                *inflight.lock() -= 1;
                let _ = tx.send(());
            });
        }
        drop(tx);
        let _ = await_receipts(&rx, cap * 3, Instant::now() + Duration::from_secs(15));
        let seen = *max_seen.lock();
        assert!(
            seen <= cap,
            "concurrent sims {seen} exceeded the budget slot cap {cap}"
        );
    }

    /// Census (PE4FPM/FPNT36): the booted host self-registers the fleet
    /// `SimDriver` resource row with the fleet seat naming.
    #[test]
    fn the_booted_host_registers_the_fleet_simdriver_census_row() {
        let _executor = FleetSimExecutor::boot(hermetic_boot()).expect("fleet sim boot");
        let row = degenbot_core::worker_census::snapshot()
            .into_iter()
            .find(|e| e.resource == degenbot_workers::role::WorkerRole::SimDriver.census_resource())
            .expect("fleet SimDriver slots must self-register in the worker census");
        assert_eq!(row.thread_name, "work-fleet-sim-{n}");
        assert_eq!(row.count, degenbot_workers::budget::DEFAULT_SIM_SLOT_CAP);
    }

    /// A quota below the pinned-role floor fails loudly at boot (the §5
    /// fail-fast — never a runtime throttle storm).
    #[test]
    fn a_budget_refusal_fails_loudly_at_boot() {
        // FF-T4 (Z6XTDX): the 2-5-core tier BOOTS the serial binding
        // now (the loud refusal moved below the serial floor — a
        // sub-2-core host cannot host the tier at all).
        let serial = FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 4.5,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: Some(hermetic_owner()),
        };
        let executor = FleetSimExecutor::boot(serial)
            .expect("a 4.5-core auto host boots the serial tier (FF-T4)");
        assert_eq!(
            executor.host_plan_binding(),
            degenbot_workers::plan::Binding::Serial,
            "the 4.5-core auto host resolves the serial tier"
        );
        drop(executor);
        // The loud refusal: a sub-serial-floor host (below
        // HOST_FLOOR_CORES) never boots a narrower topology silently.
        let refused = FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 1.5,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: Some(hermetic_owner()),
        };
        assert!(
            FleetSimExecutor::boot(refused).is_err(),
            "a sub-serial-floor quota must refuse to boot loudly"
        );
    }

    /// The design-gate admission policy, BEHAVIORAL under the shared
    /// posture owner (RZEWTX; JCI2FW Part A dissolved the
    /// `CordonAdmission::Admit` descriptor arm — the role's `SimPool`
    /// cordon class + the ONE shared owner ARE the policy): a Cordoned
    /// posture still ADMITS sim intake — units submitted under cordon run
    /// to completion (the dispatcher-side sim-intake floor paces grants;
    /// it never holds a submission, never drops one).
    #[test]
    fn a_cordoned_posture_still_admits_sim_intake() {
        let owner = hermetic_owner();
        let executor =
            FleetSimExecutor::boot(hermetic_boot_with_owner(owner)).expect("fleet sim boot");
        // Force the shared owner Cordoned (the event-burst trigger the
        // dispatcher fixtures use).
        owner.observe_throttle(
            0,
            ThrottleSample {
                events: 3,
                throttled_usec: 0,
                elapsed_usec: 100_000,
            },
        );
        assert_eq!(owner.current(), FleetPosture::Cordoned);
        let (tx, rx) = mpsc::channel::<u64>();
        for id in 0..4_u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let got = await_receipts(&rx, 4, Instant::now() + Duration::from_secs(10));
        assert_eq!(
            got.len(),
            4,
            "a Cordoned posture still admits (floors, never holds) SimPool sim intake"
        );
    }
}
