//! Dispatcher/host unit tests: bounded queues, precedence, loud overflow,
//! cordon intake, census + gauge integration.
#![expect(clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::posture::{FleetPosture, PostureOwner, PosturePolicy};

fn policy() -> PosturePolicy {
    PosturePolicy {
        enter_events: 2,
        enter_window_ms: 1_000,
        duty_percent: 2.0,
        duty_window_ms: 5_000,
        exit_clean_ms: 10_000,
        sim_intake_floor_override: None,
    }
}

/// A FRESH hermetic posture owner (leaked to `'static`): every test host
/// gets its own owner, never the process global (7KAPBB isolation).
fn hermetic_owner() -> &'static PostureOwner {
    std::boxed::Box::leak(std::boxed::Box::new(PostureOwner::new(policy())))
}

fn boot() -> FleetBoot {
    FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 8.0,
        overrides: BudgetOverrides::default(),
        posture: policy(),
        owner: Some(hermetic_owner()),
    }
}

fn boot_with_owner(owner: &'static PostureOwner) -> FleetBoot {
    FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 8.0,
        overrides: BudgetOverrides::default(),
        posture: policy(),
        owner: Some(owner),
    }
}

fn host() -> FleetHost {
    FleetHost::boot(boot()).expect("8-core boot")
}

#[test]
fn boot_fails_loudly_on_an_unhostable_quota() {
    // FF-T4: the 2-5-core tier BOOTS the serial plan — the
    // loud refusal moved below the serial floor (HOST_FLOOR_CORES: a
    // 1.5-core host cannot host ANY tier).
    let host = FleetHost::boot(FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 4.5,
        overrides: BudgetOverrides::default(),
        posture: policy(),
        owner: Some(hermetic_owner()),
    })
    .expect("a 4.5-core auto host boots the serial tier (FF-T4)");
    assert_eq!(host.plan().binding, crate::plan::Binding::Serial);
    let err = FleetHost::boot(FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 1.5,
        overrides: BudgetOverrides::default(),
        posture: policy(),
        owner: Some(hermetic_owner()),
    })
    .expect_err("H+A+R+M+2 > 1");
    assert!(matches!(err, BootError::Budget(_)));
}

#[test]
fn boot_pins_exactly_one_merge_and_registers_the_census() {
    let host = host();
    assert_eq!(host.budget().declared_sum(), host.budget().quota_floor);
    let merge = host.merge_slot();
    assert_eq!(
        host.slot_state(merge),
        Some(SlotState::Pinned {
            role: WorkerRole::Merge,
            key: MERGE_PIN_KEY
        })
    );
    // Exactly one merge pin exists.
    assert_eq!(
        host.slot_states()
            .iter()
            .filter(|(_, s)| matches!(
                s,
                SlotState::Pinned {
                    role: WorkerRole::Merge,
                    ..
                }
            ))
            .count(),
        1
    );
    // Census self-registration: the four v1-active roles are visible with
    // their budget-derived slot counts.
    let snap = degenbot_core::worker_census::snapshot();
    let budget = host.budget();
    let expected = |role: WorkerRole| match role {
        WorkerRole::Solver => budget.solver_pin_count,
        WorkerRole::SimDriver => budget.sim_slot_cap,
        WorkerRole::Resolve => usize::try_from(budget.resolve_cpus).unwrap_or(1),
        WorkerRole::Merge => usize::try_from(budget.merge_cpus).unwrap_or(1),
        WorkerRole::PoolStateUpdater => budget.pool_state_updater_slots,
        _ => 0,
    };
    for role in V1_ACTIVE_ROLES {
        let entry = snap.iter().find(|e| e.resource == role.census_resource());
        assert!(entry.is_some(), "census row missing for {role:?}");
        let entry = entry.unwrap_or(&degenbot_core::worker_census::WorkerCensusEntry {
            resource: "",
            kind: "",
            count: 0,
            thread_name: "",
            sizing: "",
            binding: "logical",
        });
        assert_eq!(entry.count, expected(role));
        assert_eq!(entry.thread_name, role.thread_name());
    }
}

/// DNZQ5G (Q1): the FSM can only reach `Pinned{k}` on ONE cell (the
/// one-seat-per-bin contract), and the derived `pin_slot` agrees with
/// that unique cell across claim and T6 cycles.
#[test]
fn exactly_one_pin_per_key_is_representable() {
    let mut host = host();
    // Claim key 7 through the dispatch path (T1→T2→T3).
    host.enqueue(Unit::noop(1, WorkerRole::Solver, Some(7)))
        .expect("queue");
    let grants = host.dispatch();
    let slot = grants
        .iter()
        .find(|(g, _)| g.kind == GrantKind::NewPinClaim)
        .expect("the cold claim is granted")
        .0
        .slot;
    host.start(slot, &Unit::noop(1, WorkerRole::Solver, Some(7)))
        .expect("T2");
    host.complete(slot).expect("T3");
    let pinned = |host: &FleetHost| {
        host.slot_states()
            .into_iter()
            .filter_map(|(s, st)| match st {
                SlotState::Pinned {
                    role: WorkerRole::Solver,
                    key,
                } => Some((s, key)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(pinned(&host), vec![(slot, 7)]);
    assert_eq!(host.pin_slot(7), Some(slot));
    // A T6 cycle (start + complete) cannot produce a second `Pinned{7}`:
    // the seat re-pins to the SAME cell.
    host.start(slot, &Unit::noop(2, WorkerRole::Solver, Some(7)))
        .expect("T6");
    host.complete(slot).expect("T3 again");
    assert_eq!(
        pinned(&host),
        vec![(slot, 7)],
        "still exactly one Pinned{{7}} after the continuation cycle"
    );
    assert_eq!(host.pin_slot(7), Some(slot));
}

/// 2SIOHJ done: `merge_slot()` reads the boot-frozen `SlotLayout`'s merge
/// index — the LAST boot slot (the boot construction pins it there
/// T1→T2→T4).
#[test]
fn merge_slot_reads_the_boot_layout() {
    let host = host();
    let merge = host.merge_slot();
    let (last, last_state) = *host.slot_states().last().expect("non-empty table");
    assert_eq!(
        merge, last,
        "the merge slot is structurally the last boot slot"
    );
    assert_eq!(
        last_state,
        SlotState::Pinned {
            role: WorkerRole::Merge,
            key: MERGE_PIN_KEY,
        }
    );
    // No second merge pin anywhere in the table.
    assert_eq!(
        host.slot_states()
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s,
                    SlotState::Pinned {
                        role: WorkerRole::Merge,
                        ..
                    }
                )
            })
            .count(),
        1
    );
}

/// the boot geometry oracle. The Q=8 production shape (VERIFIED
/// against the real derive: pins = floor(8) − 2 solve headroom = 6, sim =
/// today's `SimSlots` cap 4, resolve fixed 1, the PRG-3 station 4, the merge
/// sidecar last) tiles the table contiguously.
#[test]
fn slot_layout_matches_the_production_q8_shape() {
    let host = host();
    let layout = host.layout();
    assert_eq!(layout.solver.clone(), 0..6, "solver = the LPT bin count");
    assert_eq!(layout.sim.clone(), 6..10, "sim = today's SimSlots cap");
    assert_eq!(
        layout.resolve.clone(),
        10..11,
        "resolve = the fixed v1 seat"
    );
    assert_eq!(
        layout.poolupd.clone(),
        11..15,
        "the registration intake station"
    );
    assert_eq!(layout.merge, 15, "the merge sidecar is the LAST index");
    assert_eq!(host.slot_states().len(), 16, "the ranges tile the table");
    assert_eq!(layout.solver.end, layout.sim.start);
    assert_eq!(layout.sim.end, layout.resolve.start);
    assert_eq!(layout.resolve.end, layout.poolupd.start);
    assert_eq!(layout.poolupd.end, layout.merge);
}

/// The `first_idle_of` replica bug story (the replica since died): the
/// layout oracle says the merge sidecar is the LAST index (15 at Q=8);
/// the replica — asked for the Merge home — returned the first
/// `PoolStateUpdater` seat (11), its else-arm misclassifying every poolupd
/// slot as Merge. The replica died instead of the layout.
#[test]
fn slot_layout_pins_the_merge_sidecar_to_the_last_index() {
    let host = host();
    let layout = host.layout();
    let (last, last_state) = *host.slot_states().last().expect("non-empty table");
    assert_eq!(
        u64::try_from(layout.merge).unwrap_or(SlotId::MAX),
        last,
        "layout.merge IS the last table index"
    );
    assert_eq!(last, 15, "production Q=8: merge is the 16th slot");
    assert_eq!(
        last_state,
        SlotState::Pinned {
            role: WorkerRole::Merge,
            key: MERGE_PIN_KEY,
        }
    );
    // RED evidence (2SIOHJ, captured before the replica died): the stale
    // `first_idle_of` replica returned 11 — the first PoolStateUpdater
    // seat, misclassified as Merge by its else-arm — against this
    // oracle's 15. The replica died; the layout stands.
}

/// the 1-bin edge — `solve_headroom` 5 at Q=6 sizes exactly one
/// LPT pin (`max(1, floor(Q) − headroom)`); the layout still boots with
/// every other hosted range intact and merge last.
#[test]
fn slot_layout_of_accepts_the_one_bin_edge() {
    let host = FleetHost::boot(FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 6.0,
        overrides: BudgetOverrides {
            solve_headroom: Some(5),
            ..BudgetOverrides::default()
        },
        posture: policy(),
        owner: Some(hermetic_owner()),
    })
    .expect("the one-bin edge boots");
    let layout = host.layout();
    assert_eq!(layout.solver.clone(), 0..1, "exactly one LPT bin seat");
    assert_eq!(layout.sim.clone(), 1..5);
    assert_eq!(layout.resolve.clone(), 5..6);
    assert_eq!(layout.poolupd.clone(), 6..10);
    assert_eq!(layout.merge, 10, "merge is still the LAST index");
    assert_eq!(host.slot_states().len(), 11);
}

/// a dead station — a v1-hosted role sized to ZERO slots — is a
/// loud `BootError::Invariant` at boot, never a silently unhostable
/// station.
#[test]
fn a_dead_station_boot_is_a_loud_invariant() {
    for (name, overrides) in [
        (
            "pool_state_updater_slots = 0",
            BudgetOverrides {
                pool_state_updater_slots: Some(0),
                ..BudgetOverrides::default()
            },
        ),
        (
            "sim_slot_cap = 0",
            BudgetOverrides {
                sim_slot_cap: Some(0),
                ..BudgetOverrides::default()
            },
        ),
    ] {
        let err = FleetHost::boot(FleetBoot {
            profile: degenbot_config::FleetProfile::Auto,
            quota_cpus: 8.0,
            overrides,
            posture: policy(),
            owner: Some(hermetic_owner()),
        })
        .expect_err(name);
        assert!(
            matches!(err, BootError::Invariant(_)),
            "{name} must refuse as a boot invariant: {err:?}"
        );
    }
}

#[test]
fn merge_is_never_queued_and_declared_roles_are_gated() {
    let mut host = host();
    assert_eq!(
        host.enqueue(Unit::noop(1, WorkerRole::Merge, None)),
        Err(EnqueueError::MergeNeverQueued)
    );
    for role in [WorkerRole::Registrar, WorkerRole::Submitter] {
        assert_eq!(
            host.enqueue(Unit::noop(2, role, None)),
            Err(EnqueueError::RoleNotActive(role))
        );
    }
}

/// RED→GREEN : a keyed Solver unit whose pin is claimed
/// (in-flight or running on its pinned seat) must WAIT for its own seat —
/// the dispatch lanes must never grant a hot key as a new pin claim onto a
/// second idle seat (the pin IS the key; a double grant breaks the
/// one-seat-per-bin RAYPAR T3 contract).
#[test]
fn a_busy_pinned_key_never_grants_a_second_seat() {
    let mut host = host();
    // Claim the key-1 pin and leave it RUNNING (no T3 completion yet).
    let solver_slot =
        u64::try_from(host.layout().solver.start).expect("the layout's first Solver seat");
    host.lease_claim(solver_slot, WorkerRole::Solver, Some(1))
        .expect("T1 claim");
    host.start(solver_slot, &Unit::noop(1, WorkerRole::Solver, Some(1)))
        .expect("T2");
    // Queue: a continuation for the BUSY key 1, plus a new claim on a
    // COLD key 2, plus another continuation for key 1 behind it.
    host.enqueue(Unit::noop(10, WorkerRole::Solver, Some(1)))
        .expect("continuation (busy key)");
    host.enqueue(Unit::noop(11, WorkerRole::Solver, Some(2)))
        .expect("cold-key claim");
    host.enqueue(Unit::noop(12, WorkerRole::Solver, Some(1)))
        .expect("second continuation (busy key)");
    let grants = host.dispatch();
    let solver_grants: Vec<_> = grants
        .iter()
        .filter(|(g, _)| g.kind != GrantKind::Sim)
        .collect();
    assert_eq!(
        solver_grants.len(),
        1,
        "only the COLD key 2 may claim a seat while key 1 is hot: {solver_grants:?}"
    );
    assert_eq!(solver_grants[0].0.kind, GrantKind::NewPinClaim);
    assert_ne!(
        solver_grants[0].0.slot, solver_slot,
        "the hot key's seat must not be touched"
    );
    // The busy key's units stay queued for their pin continuation (T6
    // after T3), not dropped and not re-seated.
    assert_eq!(host.queue_len(WorkerRole::Solver), 2);
}

#[test]
fn queue_overflow_is_loud_and_counted_never_silent() {
    let mut host = host();
    let cap = host.queue_cap(WorkerRole::SimDriver);
    assert!(cap > 0);
    for i in 0..cap {
        host.enqueue(Unit::noop(
            u64::try_from(i).unwrap_or(u64::MAX) + 10,
            WorkerRole::SimDriver,
            None,
        ))
        .expect("within the bound");
    }
    let err = host
        .enqueue(Unit::noop(9999, WorkerRole::SimDriver, None))
        .expect_err("one past the bound overflows loudly");
    assert!(matches!(err, EnqueueError::QueueFull { .. }));
    assert_eq!(host.overflow_count(), 1);
}

/// LW-T5 (Seam E): precedence VISIBLE at the submit surface — sims and
/// solves submitted together (seats idle) grant ALL sims before ANY solve
/// (dispatcher doc §4 rule 2; T6 continuations still pin to their seats).
#[test]
fn submitting_sims_and_solves_together_grants_all_sims_before_any_solve() {
    let mut host = host();
    for i in 0..3u64 {
        host.enqueue(Unit::noop(i + 20, WorkerRole::SimDriver, None))
            .expect("sim submitted");
    }
    for i in 0..3u64 {
        host.enqueue(Unit::noop(i + 40, WorkerRole::Solver, Some(i + 50)))
            .expect("solve submitted");
    }
    let grants = host.dispatch();
    assert_eq!(grants.len(), 6, "both kinds grant on idle seats");
    let last_sim = grants
        .iter()
        .rposition(|(g, _)| g.kind == GrantKind::Sim)
        .expect("sims granted");
    let first_solve = grants
        .iter()
        .position(|(g, _)| g.kind != GrantKind::Sim)
        .expect("solver grants present");
    assert!(
        last_sim < first_solve,
        "ALL sims must grant before ANY solve (§4 rule 2): {grants:?}"
    );
}

#[test]
fn sim_before_solve_at_lease_time_and_solver_pins_first_via_continuations() {
    let mut host = host();
    // Claim one solver pin (cycle-critical): T1→T2→T3 by hand.
    let solver_slot =
        u64::try_from(host.layout().solver.start).expect("the layout's first Solver seat");
    host.lease_claim(solver_slot, WorkerRole::Solver, Some(1))
        .expect("T1 claim");
    host.start(solver_slot, &Unit::noop(1, WorkerRole::Solver, Some(1)))
        .expect("T2");
    host.complete(solver_slot).expect("T3");

    // Enqueue BOTH a sim and a walk with pooled slots scarce: the sim
    // queue drains first for new intake.
    host.enqueue(Unit::noop(10, WorkerRole::SimDriver, None))
        .expect("sim");
    host.enqueue(Unit::noop(11, WorkerRole::Solver, Some(2)))
        .expect("walk claim");
    let grants = host.dispatch();
    let kinds: Vec<GrantKind> = grants.iter().map(|(g, _)| g.kind).collect();
    // The queued walk on the PINNED key 1 continues first (T6).
    // (Key 2's claim lands only after sims: enqueue a continuation now.)
    host.enqueue(Unit::noop(12, WorkerRole::Solver, Some(1)))
        .expect("continuation");
    let grants2 = host.dispatch();
    if let Some(sim_pos) = kinds.iter().position(|k| *k == GrantKind::Sim) {
        assert!(
            !kinds[..sim_pos].contains(&GrantKind::NewPinClaim),
            "new Solver intake must not precede queued sims: {kinds:?}"
        );
    }
    let continuation = grants2
        .iter()
        .find(|(g, _)| g.kind == GrantKind::PinContinuation)
        .expect("pinned key continuations grant first");
    assert_eq!(continuation.0.slot, solver_slot, "the pin IS the key");
}

// (2SIOHJ deleted the first_idle_of replica: PROVEN drifted against the
// boot-frozen SlotLayout — its else-arm misclassified every poolupd seat
// as Merge (RED: Merge home ⇒ 11, oracle ⇒ 15 at Q=8). Callers read the
// layout directly.)

#[test]
fn cordon_floors_sim_intake_but_never_cancels_in_flight() {
    let mut host = host();
    // Fill all sim slots with running units BEFORE cordon.
    let cap = host.budget().sim_slot_cap;
    for i in 0..cap {
        host.enqueue(Unit::noop(
            100 + u64::try_from(i).unwrap_or(0),
            WorkerRole::SimDriver,
            None,
        ))
        .expect("enqueue");
    }
    let grants = host.dispatch();
    assert_eq!(
        grants
            .iter()
            .filter(|(g, _)| g.kind == GrantKind::Sim)
            .count(),
        cap
    );
    for (g, unit) in &grants {
        host.start(g.slot, unit).expect("T2");
    }

    // Cordon onset.
    let change = host.observe_throttle(
        0,
        crate::posture::ThrottleSample {
            events: 3,
            throttled_usec: 0,
            elapsed_usec: 100_000,
        },
    );
    assert!(matches!(change, crate::posture::PostureChange::Entered(_)));
    assert_eq!(host.posture(), FleetPosture::Cordoned);
    // In-flight sims were never cancelled.
    assert_eq!(
        host.slot_states()
            .iter()
            .filter(|(_, s)| matches!(
                s,
                SlotState::Running {
                    role: WorkerRole::SimDriver,
                    ..
                }
            ))
            .count(),
        cap
    );
    // Complete them; intake beyond the floor is refused by the grant loop.
    for (g, _) in &grants {
        host.complete(g.slot).expect("T5");
    }
    // The floor is half the cap: queue two more sims; only the floor grants.
    let floor = host.sim_intake_cap(cap);
    for i in 0..cap {
        host.enqueue(Unit::noop(
            200 + u64::try_from(i).unwrap_or(0),
            WorkerRole::SimDriver,
            None,
        ))
        .expect("enqueue");
    }
    let granted_now = host.dispatch();
    assert_eq!(
        granted_now
            .iter()
            .filter(|(g, _)| g.kind == GrantKind::Sim)
            .count(),
        floor,
        "cordon floors new sim intake at half the cap"
    );
}

#[test]
fn solver_admission_is_gated_by_the_cpu_share() {
    let mut host = host();
    let share = usize::try_from(host.budget().solver_cpus).unwrap_or(1);
    for (offset, key) in (1_u64..=(2 * u64::try_from(share).unwrap_or(0))).enumerate() {
        host.enqueue(Unit::noop(
            300 + u64::try_from(offset).unwrap_or(0),
            WorkerRole::Solver,
            Some(key),
        ))
        .expect("enqueue");
    }
    let grants = host.dispatch();
    assert_eq!(
        grants
            .iter()
            .filter(|(g, _)| g.kind == GrantKind::NewPinClaim)
            .count(),
        share,
        "at most S walks runnable concurrently — a gated bin parks"
    );
}

#[test]
fn gauge_rows_and_the_dashboard_hook_see_the_fleet() {
    static ROWS: AtomicUsize = AtomicUsize::new(0);
    fn hook(samples: &[RoleGaugeSample]) {
        ROWS.store(samples.len(), Ordering::SeqCst);
    }
    // Process-global first-wins: the funnel assertion holds only when THIS
    // test installed the hook (parallel siblings may have won it).
    let installed = crate::gauges::set_dashboard_hook(hook);
    let host = host();
    let rows = host.role_gauges();
    assert_eq!(rows.len(), 8);
    // Busy + idle == total per role; merge is pinned at boot -> busy 1.
    for row in &rows {
        assert_eq!(row.busy() + row.idle, row.total());
    }
    let merge_row = rows
        .iter()
        .find(|r| r.role == WorkerRole::Merge)
        .expect("merge row");
    assert_eq!(merge_row.pinned, 1);
    assert_eq!(merge_row.busy(), 1);
    if installed {
        assert_eq!(ROWS.load(Ordering::SeqCst), 8);
    }
}

#[test]
fn the_stranded_pipe_trips_the_loud_abort_path() {
    let tripped = std::sync::Arc::new(AtomicUsize::new(0));
    let observer = std::sync::Arc::clone(&tripped);
    let host = FleetHost::boot(boot())
        .expect("boot")
        .with_tripwire_observer(Arc::new(move |_reason| {
            observer.fetch_add(1, Ordering::SeqCst);
        }));
    let mut host = host;
    let sim_slot =
        u64::try_from(host.layout().sim.start).expect("the layout's first SimDriver seat");
    host.lease_claim(sim_slot, WorkerRole::SimDriver, None)
        .expect("T1");
    let unit = Unit::new(1, WorkerRole::SimDriver, None, true, Box::new(|_ctx| {}));
    host.start(sim_slot, &unit).expect("T2");
    // Abandoning a result-pipe unit mid-flight trips the loud abort path...
    assert!(host.strand_unit(sim_slot).is_err());
    assert_eq!(
        tripped.load(Ordering::SeqCst),
        1,
        "tripwire fired once, loudly"
    );
    // ...while the legal shed path (T7/T8) never trips it.
    host.shed(sim_slot).expect("T7");
    host.drain_done(sim_slot).expect("T8");
    assert_eq!(tripped.load(Ordering::SeqCst), 1);
}

#[test]
fn the_intake_station_is_booted_and_census_registered() {
    let host = host();
    let slots = host.budget().pool_state_updater_slots;
    assert!(slots >= 1, "the station hosts at least one slot by default");
    // The merge pin is still structurally the LAST slot.
    let last = host.slot_states().last().expect("slots").1;
    assert!(
        matches!(
            last,
            SlotState::Pinned {
                role: WorkerRole::Merge,
                ..
            }
        ),
        "merge pin is the last slot"
    );
    // Census row registered with the duty-counted budget.
    let snap = degenbot_core::worker_census::snapshot();
    let entry = snap
        .iter()
        .find(|e| e.resource == WorkerRole::PoolStateUpdater.census_resource())
        .expect("census row for the intake station");
    assert_eq!(entry.count, slots);
    assert_eq!(
        entry.thread_name,
        WorkerRole::PoolStateUpdater.thread_name()
    );
}

#[test]
fn intake_units_admit_nominal_and_held_while_cordoned() {
    let mut host = host();
    host.enqueue(Unit::noop(1, WorkerRole::PoolStateUpdater, None))
        .expect("nominal intake admits");

    // Cordon onset (same trigger the sim fixture uses).
    let change = host.observe_throttle(
        0,
        crate::posture::ThrottleSample {
            events: 3,
            throttled_usec: 0,
            elapsed_usec: 100_000,
        },
    );
    assert!(matches!(change, crate::posture::PostureChange::Entered(_)));
    host.enqueue(Unit::noop(2, WorkerRole::PoolStateUpdater, None))
        .expect_err("Deferrable intake is held while cordoned");
}

#[test]
fn intake_grants_run_behind_solve_sim_and_resolve_precedence() {
    let mut host = host();
    // Queue one unit for EVERY grant-step role plus the station.
    host.enqueue(Unit::noop(1, WorkerRole::SimDriver, None))
        .expect("sim");
    host.enqueue(Unit::noop(2, WorkerRole::Solver, Some(0x10)))
        .expect("solver");
    host.enqueue(Unit::noop(3, WorkerRole::Resolve, None))
        .expect("resolve");
    host.enqueue(Unit::noop(4, WorkerRole::PoolStateUpdater, None))
        .expect("intake");
    let grants = host.dispatch();
    let kinds: Vec<GrantKind> = grants.iter().map(|(g, _)| g.kind).collect();
    let poolupd_pos = kinds
        .iter()
        .position(|k| *k == GrantKind::PoolStateUpdate)
        .expect("the intake unit was granted");
    assert_eq!(
        grants
            .iter()
            .filter(|(g, _)| g.kind == GrantKind::PoolStateUpdate)
            .count(),
        1,
        "one intake grant for one queued unit"
    );
    // Every higher-precedence grant precedes the intake grant.
    assert!(
        kinds[..poolupd_pos]
            .iter()
            .all(|k| *k != GrantKind::PoolStateUpdate),
        "intake grant is last"
    );
}

#[test]
fn the_intake_queue_is_bounded_per_role() {
    let host = host();
    let slots = host.budget().pool_state_updater_slots;
    assert_eq!(
        host.queue_cap(WorkerRole::PoolStateUpdater),
        slots * 2,
        "the per-role bound is 2x the slot cap (same rule as sim)"
    );
}

/// The `PoolStateUpdater` home range start (the boot-frozen `SlotLayout`
/// owns the geometry, 2SIOHJ — poolupd seats boot Idle).
fn idle_intake_slot(host: &FleetHost) -> u64 {
    u64::try_from(host.layout().poolupd.start).unwrap_or(u64::MAX)
}

/// The flap window's completion end (JCI2FW Part A + the T7/T8 contract):
/// a Running deferrable unit shed at cordon onset reports `SeatDone` like
/// any other — `complete` must route the `DrainComplete` row (T8). A
/// rejected completion is a loud abort in production ("seat completion
/// (T5)" → stranded receipt pipe).
#[test]
fn the_seatdone_of_a_shed_running_unit_lands_on_t8() {
    let mut host = host();
    let slot = idle_intake_slot(&host);
    host.lease_claim(slot, WorkerRole::PoolStateUpdater, None)
        .expect("T1");
    host.start(slot, &Unit::noop(1, WorkerRole::PoolStateUpdater, None))
        .expect("T2");

    // Cordon onset sheds the in-flight deferrable unit; the seat keeps
    // executing it ("the unit always completes").
    host.shed(slot).expect("T7");
    assert!(matches!(
        host.slot_state(slot),
        Some(SlotState::Draining {
            role: WorkerRole::PoolStateUpdater
        })
    ));

    // The seat's completion (SeatDone) must retire the slot: T8 → Idle.
    let completion = host
        .complete(slot)
        .expect("a shed unit's SeatDone completes — never a completion refusal");
    assert!(matches!(completion, Completion::BackToIdle));
    assert_eq!(host.slot_state(slot), Some(SlotState::Idle));
}

/// JCI2FW Part A: the T7 shed is driven by the SHARED owner's transition
/// feed — the host drains its boot-time watch (the transition edge) and,
/// at every grant pass, the live posture (check-before-each-grant), so a
/// cordon published by ANY feeder sheds this host's deferrable in-flight
/// units promptly (they always complete, T8; pins/merge never shed).
#[test]
fn the_t7_shed_is_driven_by_the_shared_owner_transition_feed() {
    let owner = hermetic_owner();
    let mut host = FleetHost::boot(boot_with_owner(owner)).expect("8-core boot");
    let slot = idle_intake_slot(&host);
    host.lease_claim(slot, WorkerRole::PoolStateUpdater, None)
        .expect("T1");
    host.start(slot, &Unit::noop(1, WorkerRole::PoolStateUpdater, None))
        .expect("T2");

    // The owner's watch carries the transition; the host's shed loop
    // drained the deferrable in-flight unit on the same feed.
    let watch = owner.subscribe();
    let change = host.observe_throttle(
        0,
        crate::posture::ThrottleSample {
            events: 3,
            throttled_usec: 0,
            elapsed_usec: 100_000,
        },
    );
    assert!(matches!(change, crate::posture::PostureChange::Entered(_)));
    assert_eq!(watch.take_if_changed(), Some(FleetPosture::Cordoned));
    assert_eq!(watch.take_if_changed(), None, "one edge per transition");
    assert_eq!(host.posture(), FleetPosture::Cordoned);
    assert!(matches!(
        host.slot_state(slot),
        Some(SlotState::Draining {
            role: WorkerRole::PoolStateUpdater
        })
    ));
    host.drain_done(slot)
        .expect("T8: the shed unit always completes back to idle");

    // A SECOND host sharing the owner: admission consults the SAME owner
    // LIVE — mid-cordon it cannot even enqueue a deferrable unit (the gate
    // reads the owner; no per-host machine to drift), and its grant pass
    // drains the transition feed without incident (check-before-each-
    // grant; nothing deferrable is ADMITTED under cordon — the T1
    // ctx already blocks the lease — so the pass is a no-op).
    let mut host2 = FleetHost::boot(boot_with_owner(owner)).expect("8-core boot");
    assert_eq!(host2.posture(), FleetPosture::Cordoned);
    assert_eq!(
        host2.enqueue(Unit::noop(2, WorkerRole::PoolStateUpdater, None)),
        Err(EnqueueError::PostureHeld(WorkerRole::PoolStateUpdater)),
        "admission consults the shared owner — mid-cordon deferrable intake is held"
    );
    let _ = host2.dispatch();
    // Lift the cordon on the shared owner; host2's admission follows
    // immediately (the exit edge lands on its watch and the gate reads
    // the owner live).
    let mut now = 1_000;
    loop {
        owner.observe_throttle(
            now,
            crate::posture::ThrottleSample {
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
    host2
        .enqueue(Unit::noop(3, WorkerRole::PoolStateUpdater, None))
        .expect("nominal admission the moment the shared cordon lifts");
    let grants = host2.dispatch();
    assert_eq!(
        grants
            .iter()
            .filter(|(g, _)| g.kind == GrantKind::PoolStateUpdate)
            .count(),
        1,
        "the held-back unit is granted once the shared posture admits"
    );
}

#[test]
fn hermetic_owners_are_injected_never_the_process_global() {
    // Two hosts booted from DIFFERENT fresh owners hold independent
    // postures: cordoning one never moves the other (7KAPBB isolation).
    let owner_a = hermetic_owner();
    let owner_b = hermetic_owner();
    let mut host_a = FleetHost::boot(boot_with_owner(owner_a)).expect("8-core boot");
    let host_b = FleetHost::boot(boot_with_owner(owner_b)).expect("8-core boot");
    assert_ne!(
        std::ptr::from_ref(owner_a),
        std::ptr::from_ref(owner_b),
        "each hermetic boot carries its own owner"
    );
    let change = host_a.observe_throttle(
        0,
        crate::posture::ThrottleSample {
            events: 3,
            throttled_usec: 0,
            elapsed_usec: 100_000,
        },
    );
    assert!(matches!(change, crate::posture::PostureChange::Entered(_)));
    assert_eq!(host_a.posture(), FleetPosture::Cordoned);
    assert_eq!(host_b.posture(), FleetPosture::Nominal);
    assert_eq!(
        host_b.posture(),
        owner_b.current(),
        "host b consults ITS owner, not host a's"
    );
}
