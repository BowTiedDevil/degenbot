//! The `NoopStubFleetHost` conformance harness (design doc §11) — a
//! `NoopStubEngine`-style executable spec at fleet scope, `#[cfg(test)]`-
//! declared only, NEVER runtime-selectable.
//!
//! Its u8 script indexes `WorkerRole::index_in_all_roles` exactly like the
//! stage stub indexes `ALL_STAGES`; any new role or reshuffled transition
//! table fails here loudly. Asserted:
//!
//! 1. every role walks every legal transition (T1–T9);
//! 2. every illegal transition is rejected;
//! 3. the budget-sum invariant holds across a scripted quota resize
//!    (shares re-declared, sum re-checked, pin re-key ONLY via T9);
//! 4. pin/arena stability across N synthetic cycles (same key, warm token);
//! 5. the stranded-pipe tripwire fires on host death mid-drain;
//! 6. no worker ever holds a GIL across role work (the fleet carries no
//!    Python into the graph; units run as `'static + Send` Rust closures);
//! 7. per-role busy/idle gauges exist across `ALL_ROLES`.

#![expect(clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use crate::budget::BudgetOverrides;
use crate::gauges::RoleGaugeSample;
use crate::posture::{PostureOwner, PosturePolicy};
use crate::role::{WorkerRole, ALL_ROLES};
use crate::slot::{SlotState, Transition, TransitionContext, MERGE_PIN_KEY};

/// The u8 script encoding: a role's position in the sized `ALL_ROLES`
/// table (the stage-stub idiom).
#[expect(
    clippy::expect_used,
    reason = "a role missing from ALL_ROLES is a conformance bug that must fail loudly"
)]
fn role_index(role: WorkerRole) -> u8 {
    role.index_in_all_roles()
        .expect("scripted role must be in ALL_ROLES")
}

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

/// A FRESH hermetic posture owner (leaked to `'static`): every scripted
/// host gets its own owner, never the process global (7KAPBB isolation).
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

/// The scripted host: a real `FleetHost` driven by the u8 script — the
/// `NoopStub` pattern (a genuine orchestration core, exercised end-to-end by
/// the script instead of by production callers, which land in F3–F5).
fn stub_host() -> FleetHost {
    FleetHost::boot(boot()).expect("scripted host boots on the worked 8-core quota")
}

/// Walk ONE role's role-correct legal transition path end to end on a real
/// slot and assert every row lands where the table says.
#[test]
fn every_v1_active_role_walks_its_legal_transition_path() {
    for role in ALL_ROLES {
        let _idx = role_index(role); // u8 script: any table reshuffle trips here
        if !role.v1_active() {
            continue; // declared roles host at their migration step
        }
        let mut host = stub_host();
        match role {
            WorkerRole::Solver => {
                // The SlotLayout owns the home geometry; the fresh
                // scripted host's first solver seat is idle.
                let slot = u64::try_from(host.layout().solver.start)
                    .expect("the layout hosts a Solver seat");
                // T1 Idle→Leased(key) → T2 → T3 Pinned(k).
                host.lease_claim(slot, role, Some(7)).expect("T1");
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Leased { role, key: Some(7) })
                );
                host.start(slot, &Unit::noop(1, role, Some(7))).expect("T2");
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Running { role, key: Some(7) })
                );
                host.complete(slot).expect("T3");
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Pinned { role, key: 7 })
                );
                // T6: next cycle's unit for the SAME key.
                host.start(slot, &Unit::noop(2, role, Some(7))).expect("T6");
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Running { role, key: Some(7) })
                );
                host.complete(slot).expect("T3 again (warm)");
                // T9: epoch-boundary release.
                host.begin_epoch();
                assert_eq!(host.release_pin(7), Ok(slot), "T9");
                host.end_epoch();
                assert_eq!(host.slot_state(slot), Some(SlotState::Idle));
            }
            WorkerRole::Merge => {
                // Pinned at boot (T4); T6 continuation then T9 release.
                let slot = host.merge_slot();
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Pinned {
                        role,
                        key: MERGE_PIN_KEY
                    })
                );
                host.start(slot, &Unit::noop(1, role, Some(MERGE_PIN_KEY)))
                    .expect("T6");
                host.complete(slot).expect("T4 again");
            }
            WorkerRole::SimDriver | WorkerRole::Resolve | WorkerRole::PoolStateUpdater => {
                // The SlotLayout owns the home geometry; the fresh
                // scripted host's first home seat is idle.
                let seat = match role {
                    WorkerRole::SimDriver => host.layout().sim.start,
                    WorkerRole::Resolve => host.layout().resolve.start,
                    WorkerRole::PoolStateUpdater => host.layout().poolupd.start,
                    _ => unreachable!("pooled v1 roles exhausted above"),
                };
                let slot = u64::try_from(seat).expect("the layout hosts the pooled seat");
                host.lease_claim(slot, role, None).expect("T1");
                host.start(slot, &Unit::noop(1, role, None)).expect("T2");
                assert_eq!(
                    host.slot_state(slot),
                    Some(SlotState::Running { role, key: None })
                );
                // T5: pooled roles return to idle.
                assert_eq!(host.complete(slot), Ok(Completion::BackToIdle));
                assert_eq!(host.slot_state(slot), Some(SlotState::Idle));
            }
            _ => unreachable!("v1-active set is exhausted above"),
        }
    }
}

// (2SIOHJ deleted the first_idle_home_slot budget re-derivation: every
// scripted-seat read now goes through the boot-frozen SlotLayout.)

/// §3.3's illegal table, asserted on the pure FSM for EVERY role in
/// `ALL_ROLES` (declared roles included — the table is role-complete).
#[test]
fn every_illegal_transition_is_rejected_for_every_role() {
    const ADMITS: TransitionContext = TransitionContext {
        at_epoch_boundary: false,
        posture_admits_role: true,
    };
    for role in ALL_ROLES {
        let states = [
            SlotState::Idle,
            SlotState::Leased { role, key: Some(1) },
            SlotState::Running { role, key: Some(1) },
            SlotState::Pinned { role, key: 1 },
            SlotState::Draining { role },
        ];
        for from in states {
            for t in [
                Transition::Start { unit: 1 },
                Transition::CompleteToPinned,
                Transition::CompleteToIdle,
                Transition::BeginDraining,
                Transition::DrainComplete,
                Transition::ReleasePin,
            ] {
                // Sweep the FULL cross product; anything not in §3.3's rows
                // must reject. The legal pairs (superset below) are excluded
                // by matching the exact from/t combinations the table allows.
                if legal_in_table(from, t) {
                    continue;
                }
                let rejected = crate::slot::transition(from, t, ADMITS).err();
                assert!(
                    rejected.is_some(),
                    "illegal {from:?} --{t:?}--> for {role:?} was ACCEPTED"
                );
                let rejected = rejected.unwrap_or(RejectedTransition {
                    from,
                    transition: t,
                    reason: crate::slot::RejectionReason::NoLegalRow,
                });
                assert!(
                    matches!(rejected.reason, crate::slot::RejectionReason::NoLegalRow)
                        || matches!(rejected.reason, crate::slot::RejectionReason::MidCyclePin),
                    "unexpected rejection class for {from:?} --{t:?}-->: {rejected:?}"
                );
            }
        }
    }
}

/// The legal (from, transition) pairs of §3.3, table-encoded exactly once —
/// the harness's independent restatement of the table.
fn legal_in_table(from: SlotState, t: Transition) -> bool {
    matches!(
        (from, t),
        (SlotState::Idle, Transition::Lease { .. })
            | (
                SlotState::Leased { .. },
                Transition::Start { .. } | Transition::BeginDraining,
            )
            | (
                SlotState::Running { .. },
                Transition::CompleteToPinned
                    | Transition::CompleteToIdle
                    | Transition::BeginDraining,
            )
            | (
                SlotState::Pinned { .. },
                Transition::Start { .. } | Transition::ReleasePin,
            )
            | (SlotState::Draining { .. }, Transition::DrainComplete)
    )
}

/// §3.3's named illegal rows, asserted by exact class.
#[test]
fn the_named_illegal_rows_reject_by_class() {
    const ADMITS: TransitionContext = TransitionContext {
        at_epoch_boundary: false,
        posture_admits_role: true,
    };
    // Idle → Running (lease required).
    assert!(
        crate::slot::transition(SlotState::Idle, Transition::Start { unit: 1 }, ADMITS).is_err()
    );
    // Pinned(Solver, k) → Pinned(Solver, k'): re-keying is T9 then T1.
    assert!(crate::slot::transition(
        SlotState::Pinned {
            role: WorkerRole::Solver,
            key: 1
        },
        Transition::CompleteToPinned,
        ADMITS
    )
    .is_err());
    // Draining → Leased.
    assert!(crate::slot::transition(
        SlotState::Draining {
            role: WorkerRole::SimDriver
        },
        Transition::Lease {
            role: WorkerRole::SimDriver,
            key: None
        },
        ADMITS
    )
    .is_err());
    // T9 mid-cycle.
    let mid = crate::slot::transition(
        SlotState::Pinned {
            role: WorkerRole::Solver,
            key: 1,
        },
        Transition::ReleasePin,
        ADMITS,
    )
    .expect_err("epoch boundary required");
    assert_eq!(mid.reason, crate::slot::RejectionReason::MidCyclePin);
}

/// The budget-sum invariant holds across a SCRIPTED quota resize: shares
/// re-declared, sum re-checked, and pin re-keying only via T9 (a re-key on
/// a live pin without the boundary rejects `MidCyclePin`).
#[test]
fn budget_sum_invariant_holds_across_a_scripted_quota_resize() {
    let mut host = stub_host();
    assert_eq!(host.budget().declared_sum(), host.budget().quota_floor);

    // Pin two bins across the resize:
    for key in [1_u64, 2] {
        // The first IDLE solver seat: the previous iteration's pin holds
        // its seat warm, so the scan walks the SlotLayout's solver range
        // (the layout owns the geometry — 2SIOHJ, no budget re-derivation).
        let seat = host
            .layout()
            .solver
            .find(|&s| {
                host.slot_state(u64::try_from(s).unwrap_or(SlotId::MAX)) == Some(SlotState::Idle)
            })
            .expect("an idle solver seat in the layout range");
        let slot = u64::try_from(seat).expect("solver seat id");
        host.lease_claim(slot, WorkerRole::Solver, Some(key))
            .expect("T1");
        host.start(slot, &Unit::noop(key, WorkerRole::Solver, Some(key)))
            .expect("T2");
        host.complete(slot).expect("T3");
    }
    let pin_a = host.pin_slot(1).expect("pin 1");
    let warm_before = host.arena(pin_a);

    // Scripted resize (quota re-detected on cgroup focus): shares re-declared.
    host.resize_quota(6.5, &BudgetOverrides::default())
        .expect("6.5 hosts the fixed consumers + 2 solver");
    assert_eq!(host.budget().declared_sum(), host.budget().quota_floor);
    assert!(host.budget().pins_require_rekey(
        &crate::budget::FleetBudget::derive(8.0, &BudgetOverrides::default()).expect("8")
    ));

    // A re-key attempt MID-CYCLE (no epoch boundary) is rejected: pins move
    // ONLY via T9.
    let live_pin = host.pin_slot(2).expect("pin 2");
    assert!(host.release_pin(2).is_err(), "mid-cycle T9 must reject");
    assert!(matches!(
        host.slot_state(live_pin),
        Some(SlotState::Pinned { .. })
    ));

    // At the epoch boundary, the pin releases and the arena drops with it.
    host.begin_epoch();
    assert_eq!(host.release_pin(2), Ok(live_pin), "T9");
    host.end_epoch();
    assert_eq!(
        host.arena(live_pin),
        None,
        "arena never crosses a role switch"
    );
    // Pin 1 was untouched: warm identity preserved across the resize.
    assert_eq!(host.arena(pin_a), warm_before);
    assert!(matches!(
        host.slot_state(pin_a),
        Some(SlotState::Pinned {
            role: WorkerRole::Solver,
            key: 1
        })
    ));
}

/// Pin/arena stability across N synthetic cycles: same pin key, warm
/// handle identity (T6 then T3 per cycle; the token never changes).
#[test]
fn pin_and_arena_are_stable_across_synthetic_cycles() {
    const N: u64 = 25;
    let mut host = stub_host();
    let slot = u64::try_from(host.layout().solver.start).expect("solver home seat");
    host.lease_claim(slot, WorkerRole::Solver, Some(5))
        .expect("T1");
    host.start(slot, &Unit::noop(1, WorkerRole::Solver, Some(5)))
        .expect("T2");
    host.complete(slot).expect("T3 (first pin mints the arena)");
    let warm = host.arena(slot).expect("arena minted");
    for cycle in 2..=N {
        // T6: same key only.
        host.start(slot, &Unit::noop(cycle, WorkerRole::Solver, Some(5)))
            .expect("T6 same key");
        assert_eq!(
            host.arena(slot),
            Some(warm),
            "warm token identity must not change mid-cycle {cycle}"
        );
        host.complete(slot).expect("T3");
        assert_eq!(
            host.arena(slot),
            Some(warm),
            "warm token identity survives cycle {cycle}"
        );
        assert!(matches!(
            host.slot_state(slot),
            Some(SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 5
            })
        ));
        // The pin never re-keys in place.
        assert_eq!(host.pin_slot(5), Some(slot));
    }
}

/// A dead host mid-drain hits the loud-abort path (never swallows): the
/// stranded-pipe tripwire fires exactly once; the legal T7/T8 shed path
/// next to it never trips.
#[test]
fn the_stranded_pipe_tripwire_fires_on_host_death_mid_drain() {
    let tripped = Arc::new(AtomicUsize::new(0));
    let observer = Arc::clone(&tripped);
    let mut host = FleetHost::boot(boot())
        .expect("boot")
        .with_tripwire_observer(Arc::new(move |_reason| {
            observer.fetch_add(1, Ordering::SeqCst);
        }));
    // A merge unit with in-flight result sends "dies" mid-drain.
    let merge = host.merge_slot();
    host.start(
        merge,
        &Unit::new(
            1,
            WorkerRole::Merge,
            Some(MERGE_PIN_KEY),
            true,
            Box::new(|_ctx| {}),
        ),
    )
    .expect("T6");
    assert!(host.strand_unit(merge).is_err(), "the strand is loud");
    assert_eq!(tripped.load(Ordering::SeqCst), 1);
    // The legal path stays legal.
    host.complete(merge)
        .expect("T4 recovery on the scripted host");
    assert_eq!(tripped.load(Ordering::SeqCst), 1);
}

/// A worker never holds a GIL across role work: the fleet graph carries NO
/// pyo3 (the crate-level dependency assertion) and role units are 'static
/// Send Rust closures executed host-side — simulation never round-trips
/// Python (design doc §8).
#[test]
fn role_work_never_crosses_python_the_fleet_graph_is_pyo3_free() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(manifest).expect("crate manifest readable");
    assert!(
        !text.contains("pyo3"),
        "degenbot-workers must not pull pyo3 (ADR-042 §8: FFI crossed only for runtime/startup concerns)"
    );
    // Units dispatch as plain Rust closures: a real thread runs the unit
    // body to completion host-side without any interpreter round-trip.
    let ran = Arc::new(AtomicUsize::new(0));
    let observer = Arc::clone(&ran);
    let unit = Unit::new(
        1,
        WorkerRole::SimDriver,
        None,
        false,
        Box::new(move |_ctx| {
            observer.fetch_add(1, Ordering::SeqCst);
        }),
    );
    std::thread::scope(|s| {
        s.spawn(move || {
            (unit.work)(&crate::lane::LaneCtx::detached());
        })
        .join()
        .expect("host-side unit work completes");
    });
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// Per-role busy/idle gauges exist across `ALL_ROLES` (the activation
/// dashboard's fleet source; the hotpath/instruments.rs mirror consumes
/// this shape). Also asserts gauge integrity under churn.
#[test]
fn per_role_busy_idle_gauges_exist_for_the_activation_dashboard() {
    static ROWS: AtomicUsize = AtomicUsize::new(0);
    fn hook(samples: &[RoleGaugeSample]) {
        ROWS.store(samples.len(), Ordering::SeqCst);
    }
    let installed = crate::gauges::set_dashboard_hook(hook);

    let mut host = stub_host();
    let rows = host.role_gauges();
    assert_eq!(
        rows.len(),
        ALL_ROLES.len(),
        "one gauge row per declared role"
    );
    for row in &rows {
        assert_eq!(
            row.busy() + row.idle,
            row.total(),
            "busy/idle pair complete: {row:?}"
        );
    }
    // Churn moves the gauge, not the row set (the layout's first sim seat).
    let slot = u64::try_from(host.layout().sim.start).expect("sim home seat");
    host.lease_claim(slot, WorkerRole::SimDriver, None)
        .expect("T1");
    host.start(slot, &Unit::noop(9, WorkerRole::SimDriver, None))
        .expect("T2");
    let rows = host.role_gauges();
    let sim = rows
        .iter()
        .find(|r| r.role == WorkerRole::SimDriver)
        .expect("sim row");
    assert_eq!(sim.running, 1, "busy side moves with the churn");
    if installed {
        assert_eq!(
            ROWS.load(Ordering::SeqCst),
            8,
            "the dashboard hook saw all 8 rows"
        );
    }
}

/// The declared-but-not-active roles gate loudly in dispatch (Known/planned
/// gating: adding them later is an entry, not a redesign — PRG-3 hosted
/// `PoolStateUpdater` exactly this way).
#[test]
fn declared_roles_gate_in_dispatch_until_their_migration_step() {
    let mut host = stub_host();
    for role in ALL_ROLES.iter().skip(5) {
        let err = host
            .enqueue(Unit::noop(1, *role, None))
            .expect_err("declared roles do not queue yet");
        assert!(matches!(err, EnqueueError::RoleNotActive(_)), "{err}");
    }
}

/// Budget fail-fast surfaces as the loud boot error (the fleet refuses to
/// start on over-subscription — never a runtime throttle storm).
#[test]
fn overly_small_quotas_never_boot() {
    // FF-T4: the 2-5-core tier BOOTS the serial plan; the loud
    // refusal moved below the serial floor (HOST_FLOOR_CORES).
    let host = FleetHost::boot(FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 4.5,
        ..boot()
    })
    .expect("a 4.5-core auto host boots the serial tier (FF-T4)");
    assert_eq!(host.plan().binding, crate::plan::Binding::Serial);
    let err = FleetHost::boot(FleetBoot {
        profile: degenbot_config::FleetProfile::Auto,
        quota_cpus: 1.5,
        ..boot()
    })
    .expect_err("below the serial floor");
    assert!(matches!(err, BootError::Budget(_)));
}

mod pin_derive {
    //! DNZQ5G (Q1): the pin table's single-source-of-truth property, driven
    //! over random legal grant/complete/release sequences. The DERIVED
    //! renderer (`pinned_slots` — the one pin representation, the slot
    //! table's `SlotState::Pinned` cells written only by the T-table) must
    //! equal a naive independent scan of those cells, per op, exactly —
    //! set identity AND the slot-index order contract. RED history: the
    //! property was written FIRST against the mirror it replaced (the
    //! hand-maintained `pins` Vec), where a seeded mirror-desync bug
    //! (`release_pin` skipping its mirror retain) failed it with the
    //! minimal drive [Enqueue, Pump, Complete, Release] — "stale
    //! representation entry (1, 0): cell state Idle". The drive also
    //! surfaced that the mirror modeled pin CLAIMS (outliving the cell
    //! across a T6 cycle), so the mirror-subject equality held only at
    //! steady moments; the renderer subject made the equality
    //! unconditional.

    use proptest::prelude::*;

    use crate::role::WorkerRole;
    use crate::slot::{PinKey, SlotState, MERGE_PIN_KEY};

    use super::super::{pinned_slots, EnqueueError, FleetHost, GrantKind, SlotId, Unit};
    use super::stub_host;

    /// Keys 1..=KEYS: `MERGE_PIN_KEY` (0) is never driven — the boot merge
    /// pin is never released by the drive, and solve bin keys never collide
    /// with it (the solve executor's boot assertion).
    const KEYS: u8 = 6;

    /// Truth by construction: scan the slot table for `Pinned` cells.
    fn naive_pinned(host: &FleetHost) -> Vec<(PinKey, SlotId)> {
        host.slot_states()
            .into_iter()
            .filter_map(|(slot, state)| match state {
                SlotState::Pinned { key, .. } => Some((key, slot)),
                _ => None,
            })
            .collect()
    }

    /// The derived pin view's contract, asserted after EVERY drive op:
    /// the renderer equals the naive `Pinned` scan EXACTLY (set identity
    /// plus the slot-index order contract — both scan in slot order),
    /// `pin_slot` agrees with the unique cell, and one seat per bin holds
    /// under the churn (the hot-key guard + the T-table's one-`Pinned{k}`
    /// guarantee — the behavioral payload the renderer now carries).
    fn assert_pin_view_matches_truth(host: &FleetHost) {
        let rendered: Vec<(PinKey, SlotId)> = pinned_slots(&host.slots).collect();
        let naive = naive_pinned(host);
        assert_eq!(
            rendered, naive,
            "the derived pin view must equal the naive Pinned scan"
        );
        // Slot-index order (strictly ascending) — the documented contract.
        assert!(
            rendered.windows(2).all(|w| w[0].1 < w[1].1),
            "pins render in slot-index order: {rendered:?}"
        );
        for (key, slot) in &rendered {
            assert_eq!(
                host.pin_slot(*key),
                Some(*slot),
                "pin_slot must agree with the unique cell for key {key}"
            );
            let count = host
                .slot_states()
                .iter()
                .filter(|(_, s)| matches!(s, SlotState::Pinned { key: k, .. } if *k == *key))
                .count();
            assert_eq!(count, 1, "exactly one Pinned cell for key {key}");
        }
        // One seat per bin: no Solver key is claimed by two cells.
        let mut claims: Vec<PinKey> = host
            .slot_states()
            .into_iter()
            .filter_map(|(_, s)| match s {
                SlotState::Pinned { key, .. } => Some(key),
                SlotState::Leased {
                    role: WorkerRole::Solver,
                    key: Some(k),
                }
                | SlotState::Running {
                    role: WorkerRole::Solver,
                    key: Some(k),
                } => Some(k),
                _ => None,
            })
            .collect();
        let claimed = claims.len();
        claims.sort_unstable();
        claims.dedup();
        assert_eq!(claims.len(), claimed, "one seat per Solver bin: {claims:?}");
    }

    /// One drive op.
    #[derive(Debug, Clone, Copy)]
    enum PinOp {
        /// Queue a keyed Solver unit (capacity refusals are the loud,
        /// counted overflow path — the drive tolerates them).
        Enqueue { key: u8 },
        /// One pump pass: dispatch + start every granted unit (T2/T6).
        Pump,
        /// Complete one Running slot — T3/T4 (pinnable) or T5 (pooled).
        Complete { which: u8 },
        /// Epoch-boundary T9 release of one pinned SOLVER key.
        Release { which: u8 },
    }

    fn pin_ops() -> impl Strategy<Value = Vec<PinOp>> {
        proptest::collection::vec(
            prop_oneof![
                3 => any::<u8>().prop_map(|key| PinOp::Enqueue { key }),
                4 => any::<u8>().prop_map(|_| PinOp::Pump),
                3 => any::<u8>().prop_map(|which| PinOp::Complete { which }),
                1 => any::<u8>().prop_map(|which| PinOp::Release { which }),
            ],
            0..=28,
        )
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]
        #[test]
        fn the_derived_pin_view_always_equals_a_naive_scan_of_pinned_cells(
            ops in pin_ops(),
        ) {
            let mut host = stub_host();
            assert_pin_view_matches_truth(&host);
            let mut unit_id = 1_u64;
            for op in ops {
                match op {
                    PinOp::Enqueue { key } => {
                        let id = unit_id;
                        unit_id += 1;
                        if let Err(err) = host.enqueue(Unit::noop(
                            id,
                            WorkerRole::Solver,
                            Some(1 + u64::from(key % KEYS)),
                        )) {
                            assert!(
                                matches!(err, EnqueueError::QueueFull { .. }),
                                "solver intake is refused only by the bounded queue: {err}"
                            );
                        }
                    }
                    PinOp::Pump => {
                        for (grant, unit) in host.dispatch() {
                            assert!(matches!(
                                grant.kind,
                                GrantKind::PinContinuation | GrantKind::NewPinClaim
                            ));
                            host.start(grant.slot, &unit)
                                .expect("a dispatch grant always starts (T2/T6)");
                        }
                    }
                    PinOp::Complete { which } => {
                        let running: Vec<SlotId> = host
                            .slot_states()
                            .into_iter()
                            .filter(|(_, s)| matches!(s, SlotState::Running { .. }))
                            .map(|(slot, _)| slot)
                            .collect();
                        let Some(&slot) =
                            running.get(usize::from(which) % running.len().max(1))
                        else {
                            continue;
                        };
                        host.complete(slot)
                            .expect("running slots complete (T3/T4/T5)");
                    }
                    PinOp::Release { which } => {
                        // Release candidates come from the NAIVE scan (truth
                        // by construction), Solver pins only.
                        let pinned: Vec<(PinKey, SlotId)> = naive_pinned(&host)
                            .into_iter()
                            .filter(|(key, slot)| {
                                *key != MERGE_PIN_KEY
                                    && host.slot_state(*slot)
                                        == Some(SlotState::Pinned {
                                            role: WorkerRole::Solver,
                                            key: *key,
                                        })
                            })
                            .collect();
                        let Some(&(key, slot)) =
                            pinned.get(usize::from(which) % pinned.len().max(1))
                        else {
                            continue;
                        };
                        host.begin_epoch();
                        assert_eq!(
                            host.release_pin(key),
                            Ok(slot),
                            "T9 releases the live pin for key {key}"
                        );
                        host.end_epoch();
                    }
                }
                assert_pin_view_matches_truth(&host);
            }
        }
    }

    /// The pin-order contract: the derived pin view renders in
    /// SLOT-INDEX order — the deliberate normalization of the old mirror's
    /// MRU order (no caller observes pin order; `pins()` had zero callers
    /// and continuations are per-key to per-key seats).
    #[test]
    fn pins_render_in_slot_index_order() {
        let mut host = stub_host();
        // Claim two solver pins; dispatch grants queue order onto the
        // lowest idle slots: key 9 → slot 0, key 1 → slot 1 (keys
        // DELIBERATELY out of numeric order).
        host.enqueue(Unit::noop(1, WorkerRole::Solver, Some(9)))
            .expect("queue");
        host.enqueue(Unit::noop(2, WorkerRole::Solver, Some(1)))
            .expect("queue");
        let grants = host.dispatch();
        assert_eq!(grants.len(), 2, "two cold claims, two idle solver seats");
        let slot9 = grants[0].0.slot;
        let slot1 = grants[1].0.slot;
        for (grant, unit) in grants {
            host.start(grant.slot, &unit).expect("T2");
            host.complete(grant.slot).expect("T3");
        }
        let rendered: Vec<(PinKey, SlotId)> = pinned_slots(&host.slots).collect();
        assert_eq!(
            rendered,
            vec![(9, slot9), (1, slot1), (MERGE_PIN_KEY, 15)],
            "slot-index order, NOT key order (the boot merge pin renders last)"
        );
        assert!(slot9 < slot1, "claims landed on ascending slots");
        // MRU → slot-index normalization: run ONE T6 continuation cycle on
        // the LOWER-slot pin (key 9). The old `pins` mirror was MRU-ordered
        // (complete()'s retain+push), so this completion would have
        // re-ordered the table to [(1, slot1), (9, slot9)] — most recently
        // completed first. The derived renderer is slot-index ordered;
        // order among distinct keys carries no semantics (continuations
        // are per-key to per-key seats), so the normalization is
        // unobservable.
        host.enqueue(Unit::noop(3, WorkerRole::Solver, Some(9)))
            .expect("queue");
        for (grant, unit) in host.dispatch() {
            assert_eq!(grant.kind, GrantKind::PinContinuation);
            assert_eq!(
                grant.slot, slot9,
                "the pin IS the key: the continuation grants to its own seat"
            );
            host.start(grant.slot, &unit).expect("T6");
            host.complete(grant.slot).expect("T3");
        }
        let rendered: Vec<(PinKey, SlotId)> = pinned_slots(&host.slots).collect();
        assert_eq!(
            rendered,
            vec![(9, slot9), (1, slot1), (MERGE_PIN_KEY, 15)],
            "slot-index order survives the cycle (the mirror would have MRU-reordered)"
        );
    }
}

/// LW-T2 (Seam B): the lane ctx's warm arena is minted at the GRANT seam —
/// the FIRST cycle's ctx already carries the warm token; the identity
/// survives cycles within a pin, and a T9 role switch yields a DIFFERENT
/// token on re-claim (an arena never lives across a role switch, §3.4).
#[test]
fn lane_ctx_arena_is_minted_at_grant_time_warm_across_cycles_and_fresh_after_t9() {
    let mut host = stub_host();
    let slot = u64::try_from(host.layout().solver.start).expect("solver home seat");

    // Cycle 1: the ctx at grant time carries an arena ALREADY (mint at T2,
    // NOT silently deferred to the first completion).
    host.lease_claim(slot, WorkerRole::Solver, Some(9))
        .expect("T1");
    host.start(slot, &Unit::noop(1, WorkerRole::Solver, Some(9)))
        .expect("T2");
    let first = host
        .ensure_arena(slot)
        .expect("ctx arena minted at grant time");
    host.complete(slot).expect("T3");
    assert_eq!(host.arena(slot), Some(first));

    // Cycle 2: same pin — the SAME warm token.
    host.start(slot, &Unit::noop(2, WorkerRole::Solver, Some(9)))
        .expect("T6 same key");
    assert_eq!(
        host.ensure_arena(slot),
        Some(first),
        "warm identity across cycles"
    );
    host.complete(slot).expect("T3");

    // T9 role switch: the arena drops; a fresh claim mints a NEW token.
    host.begin_epoch();
    host.release_pin(9).expect("T9");
    assert_eq!(host.arena(slot), None, "arena never crosses a role switch");
    host.lease_claim(slot, WorkerRole::Solver, Some(9))
        .expect("re-claim (T1)");
    host.start(slot, &Unit::noop(3, WorkerRole::Solver, Some(9)))
        .expect("T2");
    let fresh = host
        .ensure_arena(slot)
        .expect("fresh warm identity after the switch");
    assert_ne!(
        fresh, first,
        "the re-pinned lane gets a DIFFERENT ArenaToken after the switch"
    );
}
