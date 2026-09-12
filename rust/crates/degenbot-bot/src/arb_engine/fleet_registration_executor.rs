//! Fleet-hosted registration intake executor (PRG-3 / ADR-042 F2):
//! `PoolStateUpdater`-role hosting of the registration crawl's pool-build
//! consumers — the fleet becomes the unit pool for the crawl's build work
//! under the `fleet.stance=Fleet` migration stance. The incumbent private
//! `ThreadPoolExecutor` (its own sizing rule — the per-era pile ADR-042 kills) retires onto the fleet's declared duty-counted
//! `PoolStateUpdater` slots; the legacy stance keeps the incumbent runtime
//! byte-for-byte.
//!
//! The seat pool is the budget's `pool_state_updater_slots` (default 4,
//! `fleet.pool_state_updater_slots` terminal override — the `SimDriver`
//! billing model exactly: duty-counted, spendable from the fractional
//! remainder, never part of the declared integer sum). Grants run strictly
//! BEHIND solve/sim/resolve precedence (host dispatch lane 5); a cordon
//! HOLDS intake entirely (Deferrable cordon class — `enqueue` refuses
//! while cordoned) and in-flight units are never cancelled.
//!
//! RZEWTX: the pooled-seat machinery (`WorkQueue`, `seat_loop`,
//! `host_loop`, `apply_host_msg`/`pump` admission, the boot install/global
//! boilerplate) is SHARED with the sim executor — ONE seat host
//! (`arb_engine::seat_host`) parameterized by this module's [`REG_ROLE`]
//! descriptor. 6HE6RF: the solve executor's host-MESSAGE triple joins
//! that machinery too (the ONE [`HostPump`] behind all three fleet
//! hosts); its SEAT MODEL (per-seat keyed mailboxes, warm arenas) and
//! typed submit seam stay in `fleet_solve_executor.rs` — the RZEWTX
//! design gate now covers only the seat models (see `seat_host`'s
//! module doc).
//!
//! Unit bodies are the crawl's pool-build callables: they ride the FFI at
//! the seat boundary (`Python::attach` in the closure), release the GIL
//! through the existing `py.detach` seams inside the Rust builders, and
//! return receipts over the caller's channel — the identical behavior the
//! legacy worker threads had (parity gate), now bounded by the fleet's
//! declared slots and visible in the worker census as
//! `fleet_pool_state_updater_slots`.
//!
//! Units carry no pin key (the role is pooled, T5: run → back-to-idle);
//! keyed admission is ENGINE-side (PRG-1 build flights), so the host FSM's
//! pin/merge lanes never fire here. The `WrapDatabaseAsync` runtime-capture
//! caveat (ADR-042 §8) is unchanged: build callables already enter the
//! installed hooks via the existing seams.

use std::sync::OnceLock;

use crate::arb_engine::boot_stamp::{BootRole, BootStamp};
use degenbot_workers::budget::FleetBudget;
use degenbot_workers::dispatcher::{BootError, FleetBoot, GrantKind};
use degenbot_workers::role::WorkerRole;

use std::sync::Arc;

use crate::arb_engine::fleet_intake::{FleetIntake, InnerWork, IntakeFaultWatch};
use crate::arb_engine::seat_host::{self, SeatHost, SeatRoleDesc};

/// The intake executor's seat-host role descriptor — this module IS the
/// role now; the machinery lives once in `seat_host`. `PoolStateUpdater`
/// pooled seats granted `GrantKind::PoolStateUpdate` units and the
/// budget's `pool_state_updater_slots` as the seat count. Admission is
/// the role's own cordon class through the ONE shared posture owner
/// (JCI2FW Part A — the dissolved `CordonAdmission::Hold` descriptor
/// arm): a Cordoned posture HOLDS Deferrable intake — held units wait in
/// the unbounded backlog (never dropped), in-flight units are never
/// cancelled.
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

/// The queue-cap source: the budget's `pool_state_updater_slots` (default
/// 4, `fleet.pool_state_updater_slots` terminal override — the `SimDriver`
/// billing model exactly: duty-counted, spendable from the fractional
/// remainder, never part of the declared integer sum).
fn reg_seats_of(budget: &FleetBudget) -> usize {
    budget.pool_state_updater_slots
}

/// The fleet-hosted registration intake executor. Shared by the whole
/// process (the global static hands out `&'static`, mirroring the fleet
/// sim/solve executors' construction-once contract: warm pooled seats for
/// the process lifetime).
pub struct FleetRegistrationExecutor {
    /// The shared pooled-seat host (the channel submit end + the unit
    /// sequence).
    host: SeatHost,
    /// This executor's S2 fault watch (TB4QGX T6). Per-instance so hermetic
    /// executors isolate; the process-global executor shares the process one.
    fault_watch: Arc<IntakeFaultWatch>,
    /// The budget's `PoolStateUpdater` slot cap (the pooled seat count).
    #[cfg(test)]
    seats: usize,
}

impl FleetRegistrationExecutor {
    /// Boot from a `FleetBoot` (quota + overrides + posture): boot the
    /// [`FleetHost`], spawn the pooled `PoolStateUpdater` seat threads (the
    /// budget's station slot cap, `work-fleet-poolupd-{n}` census naming),
    /// and run the dispatch loop on the host thread. Fail-loud (the typed
    /// [`BootError`]) when the declared shares cannot host the quota.
    ///
    /// # Errors
    /// [`BootError`] — the fleet budget sum check or a boot invariant.
    pub fn boot(boot: FleetBoot) -> Result<Self, BootError> {
        Self::boot_with_watch(boot, Arc::new(IntakeFaultWatch::new()))
    }

    fn boot_with_watch(
        boot: FleetBoot,
        fault_watch: Arc<IntakeFaultWatch>,
    ) -> Result<Self, BootError> {
        let host = SeatHost::boot(&REG_ROLE, boot, Some(Arc::clone(&fault_watch)))?;
        Ok(Self {
            #[cfg(test)]
            seats: host.seat_count(),
            host,
            fault_watch,
        })
    }

    /// This executor's S2 fault watch (the pyo3 receipt observes it).
    #[must_use]
    pub(crate) fn fault_watch(&self) -> Arc<IntakeFaultWatch> {
        Arc::clone(&self.fault_watch)
    }

    /// The budget's `PoolStateUpdater` slot cap (the pooled seat count).
    /// Test-facing (the intake submits without asking the cap).
    #[cfg(test)]
    #[must_use]
    pub fn seat_count(&self) -> usize {
        self.seats
    }

    /// The port's unit body (the pre-existing submit, renamed from the
    /// inherent `spawn`, reg:155-174): wraps into `Unit::new(.., Box::new(
    /// move |_ctx| work()))` and enqueues over `tx.send(HostMsg::Enqueue(
    /// unit))`, typed to the port's `Result<(), ()>` close vocabulary - the
    /// send VALUE carries the close arm; the abort lives in the trait impl
    /// (reg:173-175's `abort_executor` - today's "intake submission" /
    /// "fleet intake host channel closed" - moved there; same process-exit
    /// semantics, one owner of the abort — now the shared seat host's
    /// `intake_spawn`). `pub(crate)` fn, in-crate (T3's
    /// cross-module pin in `fleet_intake`'s tests binds the `Result<(), ()>`
    /// shape by calling it - the surface stays crate-internal, invisible to
    /// the §4.3 pub-surface grep).
    pub(crate) fn try_send(&self, work: InnerWork) -> Result<(), ()> {
        self.host.try_send(work)
    }

    /// Test-venue shim: the OLD name `spawn`, `#[cfg(test)]`-only, so the
    /// in-file fixtures keep compiling VERBATIM. Outside test builds the
    /// inherent fn does not EXIST - the inherent-priority shadow (§6 risk 6)
    /// is confined to test code that calls no port path, and the port is the
    /// only `spawn` production callers can name.
    #[cfg(test)]
    fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        let _ = self.try_send(Box::new(work));
    }
}

impl FleetIntake for FleetRegistrationExecutor {
    fn spawn(&self, work: InnerWork) {
        if self.try_send(work).is_err() {
            seat_host::intake_close_abort(&self.host);
        }
    }
}

static FLEET_REGISTRATION_BOOT: OnceLock<BootStamp> = OnceLock::new();
// FF-T1 (BPHR6F): the slot parks the sticky boot OUTCOME — a refused
// boot stores its typed BootError and every later submission re-surfaces
// it (never a process abort, never a retry loop).
static FLEET_REGISTRATION_EXECUTOR: OnceLock<Result<FleetRegistrationExecutor, BootError>> =
    OnceLock::new();

/// Install the CONSTRUCTION-STAMPED boot (YI5NGB): the engine's own typed
/// boot descriptor (fleet quota + overrides + posture) parsed at ITS
/// construction from the CALLER cfg, stamped with the engine id + a
/// deterministic cfg hash. Never overrides an installed value (first
/// engine wins, like the other stance statics) — every construction after
/// the first RIDES, and the ride is ledgered (a divergent-cfg rider is
/// counted + warned in prod, ILLEGAL in tests) on the
/// `BootRole::Registration` row (the shared installer:
/// `seat_host::install_boot`).
pub fn install_boot(stamp: BootStamp) {
    seat_host::install_boot(&FLEET_REGISTRATION_BOOT, &REG_ROLE, stamp);
}

/// Whether an engine installed a fleet boot STAMP (YI5NGB: the stamp is
/// the boot descriptor + its construction identity) — the intake is
/// hosted ONLY under the fleet stance (the legacy stance keeps the
/// incumbent `ThreadPoolExecutor` byte-for-byte). Presence semantics
/// unchanged: the construction latch and its process-level visibility
/// are the PRG-5 probe contract.
#[must_use]
pub fn boot_installed() -> bool {
    FLEET_REGISTRATION_BOOT.get().is_some()
}

/// The installed stamp's BOOT value (FF-T5, NT7HJC — the runtime
/// status's authoritative source: the boot the FIRST engine
/// construction derived from ITS OWN config). `None` pre-construction.
#[must_use]
pub fn stamped_boot() -> Option<degenbot_workers::dispatcher::FleetBoot> {
    FLEET_REGISTRATION_BOOT
        .get()
        .map(crate::arb_engine::boot_stamp::BootStamp::boot)
}

/// The process-wide fleet registration intake executor, built lazily on the
/// first fleet-stance intake submission and persisting for the process
/// lifetime. Crate-internal (LNQDOA §4.2): its only callers are the
/// `fleet_intake` facade hand-outs — the executor TYPE crosses a boundary
/// exactly once, as an anonymous trait object (the shared materializer:
/// `seat_host::global_executor` — the YI5NGB absence window stays closed
/// by construction).
///
/// FF-T1 (BPHR6F): a refused boot surfaces the TYPED, STICKY `BootError`
/// (every submission re-surfaces the same refusal) — the library never
/// aborts the host process on the boot-refusal arm; the pyo3 leaf maps
/// it onto the `BootRefused` exception and the binary owns the loud exit.
pub(crate) fn global_fleet_registration_executor(
) -> Result<&'static FleetRegistrationExecutor, BootError> {
    seat_host::global_executor(
        &REG_ROLE,
        &FLEET_REGISTRATION_BOOT,
        &FLEET_REGISTRATION_EXECUTOR,
        FleetRegistrationExecutor::boot,
    )
}

#[cfg(test)]
// The panic-survival fixture panics deliberately (loud-assert test style;
// the module-level expect is the documented-permitted form). Mirror of the
// fleet sim executor's fixture set.
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
    fn fleet_registration_materializer_without_a_stamp_is_loud() {
        if super::FLEET_REGISTRATION_BOOT.get().is_some() {
            eprintln!(
                "skipping: another test already installed the registration boot stamp in this process"
            );
            return;
        }
        let closure = || {
            let stamp = super::FLEET_REGISTRATION_BOOT.get().expect(
                "fleet registration boot stamp missing: an engine must construct before the first fleet submit (YI5NGB)",
            );
            match FleetRegistrationExecutor::boot(stamp.boot()) {
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

    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use degenbot_workers::budget::{BudgetError, BudgetOverrides};
    use degenbot_workers::dispatcher::{BootError, FleetBoot};
    use degenbot_workers::posture::{FleetPosture, PostureOwner, PosturePolicy, ThrottleSample};

    use super::FleetRegistrationExecutor;

    /// A FRESH hermetic posture owner (leaked to `'static`): every test
    /// boot gets its own owner, never the process global (7KAPBB isolation).
    fn hermetic_owner() -> &'static PostureOwner {
        std::boxed::Box::leak(std::boxed::Box::new(PostureOwner::new(
            PosturePolicy::doc_defaults(),
        )))
    }

    /// FF-T1 (BPHR6F): a refused fleet boot is a TYPED, STICKY error at
    /// the process materializer — never a process abort. A sub-floor
    /// stamp (quota 2.0, the CI 4-vCPU shape shrunk one step further) is
    /// installed directly; the FIRST materialization surfaces the typed
    /// `BootError`, the SECOND re-surfaces the SAME refusal (the sticky
    /// `OnceLock` — “at every submit”), and the test process is alive
    /// throughout (reaching the asserts IS the survival proof). Skips if
    /// another test already installed the stamp (the F1 race discipline).
    #[expect(
        clippy::print_stderr,
        reason = "the self-skip channel when a parallel test won the stamp race (the documented F1 skip semantics)"
    )]
    #[test]
    fn a_refused_boot_is_typed_and_sticky_never_an_abort() {
        if super::FLEET_REGISTRATION_BOOT.get().is_some() {
            eprintln!(
                "skipping: another test already installed the registration boot stamp in this process"
            );
            return;
        }
        let stamp = crate::arb_engine::boot_stamp::BootStamp::of(FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 2.0,
            overrides: BudgetOverrides::default(),
            posture: PosturePolicy::doc_defaults(),
            owner: None,
        });
        let _ = super::FLEET_REGISTRATION_BOOT.set(stamp);
        let first = super::global_fleet_registration_executor();
        let Err(err) = first else {
            panic!("a sub-floor boot must refuse, typed")
        };
        assert!(
            matches!(
                &err,
                BootError::Budget(BudgetError::QuotaTooSmallForPinnedRoles { quota, required })
                    if (*quota - 2.0).abs() < f64::EPSILON && *required == 6
            ),
            "the refusal must be the pinned-role floor family (budget + floor), got {err:?}"
        );
        // Sticky: the second call re-surfaces the SAME typed refusal —
        // every later submit sees it, no retry loop, no abort.
        let second = super::global_fleet_registration_executor();
        assert!(
            second.is_err(),
            "the boot refusal is sticky at every submit (FF-T1)"
        );
        // The process survived; the budget derivation is the FIRST boot
        // step, so nothing (lane, thread, pipe) was created to refuse on.
    }
    fn hermetic_boot() -> FleetBoot {
        hermetic_boot_with_owner(hermetic_owner())
    }

    /// FF-T3 (Z2YW52): no new binding is reachable from `auto` yet — the
    /// serial arm lands with FF-T4. A sub-floor auto host refuses with
    /// FF-T4 (Z6XTDX) — AC 1: serial boot on a simulated 2-core quota
    /// executes callables on the named seat, returns receipts, and
    /// balances the ledger (every submitted unit completes exactly once
    /// — the same never-drop receipt contract the pooled seats hold).
    #[test]
    fn serial_boot_executes_callables_on_the_named_seat() {
        // AUTO on a 2-core host: the plan resolves the serial tier
        // (FF-T2) and the binding seam instantiates the serial seat
        // model — ONE `work-fleet-serial-0` cycle thread over the same
        // HostPump (§10 never-drop unchanged).
        let executor = FleetRegistrationExecutor::boot(FleetBoot {
            quota_cpus: 2.0,
            profile: degenbot_config::FleetProfile::Auto,
            ..hermetic_boot()
        })
        .expect("auto on a 2-core host boots the serial binding");
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
        assert_eq!(
            got, want,
            "every serial-lane unit completes its receipt exactly once"
        );
    }

    /// FF-T4 — the named seat: serial-lane units execute ON the
    /// `work-fleet-serial-0` cycle thread (the callable itself reports
    /// its thread).
    #[test]
    fn serial_units_execute_on_the_named_serial_seat() {
        let executor = FleetRegistrationExecutor::boot(FleetBoot {
            quota_cpus: 2.0,
            profile: degenbot_config::FleetProfile::Auto,
            ..hermetic_boot()
        })
        .expect("auto on a 2-core host boots the serial binding");
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
            assert_eq!(
                name, "work-fleet-serial-0",
                "a serial-lane unit must execute on the named serial seat"
            );
        }
    }

    /// FF-T4 — the forced serial profile on a PINNED-floor host also
    /// boots serial (the operator override is honored, never a silent
    /// narrow).
    #[test]
    fn forced_serial_boots_the_serial_seat() {
        let executor = FleetRegistrationExecutor::boot(FleetBoot {
            quota_cpus: 8.0,
            profile: degenbot_config::FleetProfile::Serial,
            ..hermetic_boot()
        })
        .expect("forced serial boots the serial binding");
        let (tx, rx) = mpsc::channel::<u64>();
        for id in 0..4_u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let mut got = await_receipts(&rx, 4, Instant::now() + Duration::from_secs(10));
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3]);
    }

    /// FF-T4 — AC 5: a FORCED pinned binding on a 4-core quota runs
    /// marked-oversubscribed (the projection's marks, never a refusal).
    #[test]
    fn forced_pinned_on_four_cores_runs_marked_oversubscribed() {
        let executor = FleetRegistrationExecutor::boot(FleetBoot {
            quota_cpus: 4.0,
            profile: degenbot_config::FleetProfile::Pinned,
            ..hermetic_boot()
        })
        .expect("forced pinned boots the marked projection");
        // The projection is marked oversubscribed and still RUNS: the
        // units complete on the pooled seats.
        let (tx, rx) = mpsc::channel::<u64>();
        for id in 0..8_u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let mut got = await_receipts(&rx, 8, Instant::now() + Duration::from_secs(10));
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    /// FF-T3 (Z2YW52): the census prints the lane-to-thread binding per
    /// entry — the fleet rows stamp `pinned` (dedicated seat threads)
    /// under the pinned binding.
    #[test]
    fn the_fleet_census_rows_stamp_the_lane_to_thread_binding() {
        let executor = FleetRegistrationExecutor::boot(hermetic_boot()).expect("fleet intake boot");
        drop(executor);
        let rows = degenbot_core::worker_census::snapshot();
        let fleet_rows: Vec<_> = rows
            .iter()
            .filter(|entry| entry.resource.starts_with("fleet_"))
            .collect();
        assert!(
            !fleet_rows.is_empty(),
            "the host registers the fleet census rows"
        );
        // FF-T4 (Z6XTDX): the census is PROCESS-GLOBAL and the test
        // binary boots serial-binding hosts in parallel tests — a row
        // stamps whichever binding last registered that role. The
        // deterministic assertion is the VOCABULARY contract (FF-T2:
        // every fleet row stamps a member of {pinned, shared, logical});
        // the binding RESOLUTION determinism is the plan tests.
        for row in fleet_rows {
            assert!(
                matches!(row.binding, "pinned" | "shared" | "logical"),
                "every fleet row stamps the lane-to-thread binding vocabulary (row {})",
                row.resource
            );
        }
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

    fn await_receipts<T: Send + 'static>(
        rx: &mpsc::Receiver<T>,
        want: usize,
        deadline: Instant,
    ) -> Vec<T> {
        let mut got: Vec<T> = Vec::new();
        while got.len() < want {
            assert!(
                Instant::now() < deadline,
                "fleet intake seats did not drain {want} units in time (got {})",
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

    /// The intake seats must run each submitted build unit to completion,
    /// every receipt delivered exactly once — the never-drop contract the
    /// legacy crawl worker threads provided.
    #[test]
    fn every_submitted_intake_unit_completes_exactly_once() {
        let executor = FleetRegistrationExecutor::boot(hermetic_boot()).expect("fleet intake boot");
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
        assert_eq!(got, want, "every intake unit completes exactly once");
    }

    /// Seats are the fleet `PoolStateUpdater` role: census thread-name
    /// pattern work-fleet-poolupd-{n} (GOQWCL rule).
    #[test]
    fn intake_units_execute_on_named_fleet_poolupd_seats() {
        let executor = FleetRegistrationExecutor::boot(hermetic_boot()).expect("fleet intake boot");
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
                name.starts_with("work-fleet-poolupd-"),
                "intake unit must execute on a fleet PoolStateUpdater seat, got {name:?}"
            );
        }
    }

    /// A panicking build closure must not kill its seat (the pool would
    /// strand the awaiting crawl workers' receipts): the surviving seat
    /// still drains later units.
    #[test]
    fn a_panicking_build_leaves_the_seat_alive() {
        let executor = FleetRegistrationExecutor::boot(hermetic_boot()).expect("fleet intake boot");
        // Panic in the middle of the flood.
        executor.spawn(move || panic!("deliberate build failure"));
        let (tx, rx) = mpsc::channel::<u64>();
        for id in 0..8_u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let got = await_receipts(&rx, 8, Instant::now() + Duration::from_secs(10));
        assert_eq!(got.len(), 8, "the seat pool survived the panic and drained");
    }

    /// The cordon HOLDS Deferrable intake, but submitted units are never
    /// dropped: they wait and drain when the seat pool next gets capacity.
    /// (In-process cordon orchestration is the posture crate's fixture
    /// domain — `dispatcher::tests::intake_units_admit_nominal_and_held_...`
    /// proves the FSM hold; this executor-level test proves the backlog
    /// preserves units across a queue-full spill.)
    #[test]
    fn a_full_per_role_queue_never_drops_units() {
        let executor = FleetRegistrationExecutor::boot(hermetic_boot()).expect("fleet intake boot");
        let cap = executor.seat_count();
        let (tx, rx) = mpsc::channel::<u64>();
        // A flood far past the seat pool AND the 2x queue bound.
        for id in 0..(4 * cap + 8) as u64 {
            let tx = tx.clone();
            executor.spawn(move || {
                let _ = tx.send(id);
            });
        }
        drop(tx);
        let want = (4 * cap + 8) as u64;
        let got = await_receipts(
            &rx,
            usize::try_from(want).unwrap_or(usize::MAX),
            Instant::now() + Duration::from_secs(30),
        );
        assert_eq!(
            got.len() as u64,
            want,
            "no unit dropped across the backlog spill"
        );
    }

    /// The design-gate admission policy, BEHAVIORAL and REACHED (RZEWTX;
    /// JCI2FW Part A dissolved the `CordonAdmission::Hold` descriptor arm
    /// and made the Cordoned arm reachable): `PoolStateUpdater` is
    /// Deferrable cordon class — a forced-Cordoned hermetic owner HOLDS
    /// intake (the unit waits in the unbounded backlog, no receipt), and
    /// the held unit COMPLETES once the clean hysteresis lifts the cordon
    /// (never dropped; in-flight units are never cancelled).
    #[test]
    fn a_cordoned_posture_holds_registration_intake_until_the_cordon_lifts() {
        let owner = hermetic_owner();
        let executor =
            FleetRegistrationExecutor::boot(hermetic_boot_with_owner(owner)).expect("fleet boot");
        // Force the shared owner Cordoned (the event-burst trigger the
        // dispatcher fixtures use) — previously unreachable at this seam.
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
        let tx1 = tx.clone();
        executor.spawn(move || {
            let _ = tx1.send(1);
        });
        // The Hold arm: no receipt while the cordon holds (the unit waits
        // in the backlog; admission consulted the shared owner directly).
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            rx.try_recv().is_err(),
            "a Cordoned posture HOLDS Deferrable registration intake"
        );
        // Lift: feed the full clean hysteresis (10 s of virtual clean
        // ticks since the dirty sample at now = 0).
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
        // A fresh submission wakes the host loop: the backlog head (the
        // held unit) drains FIRST, then the new unit — both complete.
        executor.spawn(move || {
            let _ = tx.send(2);
        });
        let mut got = await_receipts(&rx, 2, Instant::now() + Duration::from_secs(10));
        got.sort_unstable();
        assert_eq!(got, vec![1, 2], "the held unit completed (never dropped)");
    }
}
