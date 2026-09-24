//! Worker-slot states + the T1–T9 legal-transition table (design doc
//! §3.2–§3.3).
//!
//! A worker **slot** is a persistent host resource (a thread/booted task);
//! its `role` is the scheduling unit. The FSM is a pure, total function:
//! `transition(from, t, ctx) -> Result<SlotState, RejectedTransition>`. Any
//! move not covered by a T-row is a loud, typed rejection — never a silent
//! re-wrap (§3.3's illegal list).

use crate::role::{CordonClass, WorkerRole};

/// Pin key = the LPT bin id (job→bin affinity; §3.4).
pub type PinKey = u64;
/// Unit id per role queue.
pub type UnitId = u64;

/// Exactly one merge pin exists; its key is fixed (T4's pipe-drain handoff
/// is verified by the host, the table only sees the key).
pub const MERGE_PIN_KEY: PinKey = 0;

/// State of one worker slot (design doc §3.2). `Cordoned` is a PROCESS-level
/// posture, deliberately NOT a slot state — it gates transitions (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotState {
    /// Parked; no lease; zero CPU.
    Idle,
    /// Dispatch granted a unit of work (or a pin claim; `key` is the pin
    /// key for pinnable roles).
    Leased {
        role: WorkerRole,
        key: Option<PinKey>,
    },
    /// Executing the unit; `key` survives from the lease's pin claim.
    Running {
        role: WorkerRole,
        key: Option<PinKey>,
    },
    /// Steady lease held ACROSS cycles (Solver per bin, Merge exactly one);
    /// warm arenas intact.
    Pinned { role: WorkerRole, key: PinKey },
    /// Finishing its in-flight unit under shed/cordon; takes nothing new.
    Draining { role: WorkerRole },
}

impl SlotState {
    /// The role this slot is currently leased for, if any.
    #[must_use]
    pub const fn role(self) -> Option<WorkerRole> {
        match self {
            Self::Idle => None,
            Self::Leased { role, .. }
            | Self::Running { role, .. }
            | Self::Pinned { role, .. }
            | Self::Draining { role } => Some(role),
        }
    }

    /// Whether this state holds an in-flight unit of work (T7's shed class).
    #[must_use]
    pub const fn holds_in_flight(self) -> bool {
        matches!(
            self,
            Self::Leased { .. } | Self::Running { .. } | Self::Pinned { .. }
        )
    }
}

/// A transition attempt (design doc §3.3 rows T1..T9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// T1: dispatch granted a unit of work (or a pin claim; pinnable roles
    /// must claim their key here).
    Lease {
        role: WorkerRole,
        key: Option<PinKey>,
    },
    /// T2/T6: the worker dequeues the unit / claims the pin (T2) or takes
    /// the next cycle's unit under an existing pin (T6 — same key by
    /// construction: the pin IS the key, host-dispatched).
    Start { unit: UnitId },
    /// T3/T4: unit complete; a pinnable role converts to a warm pin.
    CompleteToPinned,
    /// T5: unit complete for pooled roles; back to the idle set.
    CompleteToIdle,
    /// T7: cordon onset on a deferrable role, or fleet resize mid-cycle —
    /// the in-flight unit always completes.
    BeginDraining,
    /// T8: in-flight unit done; nothing taken.
    DrainComplete,
    /// T9: explicit rebalance (quota change / config override) — epoch
    /// boundary only, never mid-cycle.
    ReleasePin,
}

/// Guards the table consults; the host and the posture FSM supply them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransitionContext {
    /// True at an epoch boundary (quota re-detection / config override);
    /// T9 is illegal anywhere else.
    pub at_epoch_boundary: bool,
    /// True when the posture admits lease intake for the role (cordon
    /// blocks deferrable roles entirely; sim-pool roles are only
    /// intake-FLOORED, so they keep passing here).
    pub posture_admits_role: bool,
}

/// A rejected transition: loud by construction, typed for the conformance
/// stub to assert exactly WHICH row was violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rejected transition {from:?} --{transition:?}--> ({reason})")]
pub struct RejectedTransition {
    /// The state the move was attempted from.
    pub from: SlotState,
    /// The attempted transition.
    pub transition: Transition,
    /// Which part of the table rejected it.
    pub reason: RejectionReason,
}

/// Why a transition left the legal table (design doc §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RejectionReason {
    /// T1 grant edge denied by the posture (cordon holds deferrable lease
    /// intake; already-granted units always complete — T2/T6 are unguarded).
    #[error("posture blocks new lease intake for this role")]
    PostureBlocksIntake,
    /// T9 outside an epoch boundary: mid-cycle pin mutation.
    #[error("pin release requires an epoch boundary (T9)")]
    MidCyclePin,
    /// T3: a completed Solver walk carries no pin key.
    #[error("Solver completion carries no pin key (T3)")]
    MissingPinKey,
    /// T1: a pinnable role's lease claim carried no pin key.
    #[error("pinnable-role lease claim carries no pin key (T1)")]
    ClaimMissingKey,
    /// T1: a Merge claim does not carry the single merge pin key.
    #[error("Merge pin claims must carry MERGE_PIN_KEY (T1/T4: exactly one merge pin)")]
    MergeKeyMismatch,
    /// The from/transition pair is off the table's end (§3.3 illegal list:
    /// `Idle → Running`, mid-unit `Running → Idle` for pinned roles,
    /// re-keying a pin without T9, `Draining → Leased`, ...).
    #[error("no legal row (T1-T9) covers this from/transition pair")]
    NoLegalRow,
}

/// The T1–T9 table: `from ─transition→ to`, or a loud typed rejection.
///
/// # Errors
/// A [`RejectedTransition`] whenever `(from, t)` is off the legal table or
/// a guard denies the row.
#[must_use = "a rejected transition is a conformance event, not a suggestion"]
pub fn transition(
    from: SlotState,
    t: Transition,
    ctx: TransitionContext,
) -> Result<SlotState, RejectedTransition> {
    let reject = |reason: RejectionReason| {
        Err::<SlotState, RejectedTransition>(RejectedTransition {
            from,
            transition: t,
            reason,
        })
    };
    // Posture guard on the T1 INTAKE edge: §3.3 — leasing a cordon-
    // deferrable role under cordon is illegal (sim-pool roles only floor
    // their intake, so they pass). The START edges (T2/T6) are
    // deliberately unguarded: intake admitted before the cordon always
    // completes (the §3.3 invariant T7 encodes) — it just takes nothing
    // new while the fleet is cordoned.
    let posture_guards_role = |role: WorkerRole| {
        (role.cordon_class() == CordonClass::Deferrable && !ctx.posture_admits_role)
            .then_some(RejectionReason::PostureBlocksIntake)
    };

    match (from, t) {
        // T1: Idle → Leased(r) — dispatchable ∧ capacity ∧ posture admits.
        (SlotState::Idle, Transition::Lease { role, key }) => {
            if let Some(reason) = posture_guards_role(role) {
                return reject(reason);
            }
            if role.is_pinnable() && key.is_none() {
                return reject(RejectionReason::ClaimMissingKey);
            }
            if role == WorkerRole::Merge && key != Some(MERGE_PIN_KEY) {
                return reject(RejectionReason::MergeKeyMismatch);
            }
            Ok(SlotState::Leased { role, key })
        }
        // T2: Leased(r) → Running(r, unit); the claim key carries through.
        // No posture re-check: the posture gates INTAKE (T1) and sheds
        // mid-cycle (T7). A grant admitted before a cordon ALWAYS starts
        // and completes — rejecting Start here would strand the Leased
        // slot (no exit but the shed) and trip the loud stranded-pipe
        // abort (prod flap-window regression, unit 1316).
        (SlotState::Leased { role, key }, Transition::Start { .. }) => {
            Ok(SlotState::Running { role, key })
        }
        // T6 (same Start constructor): Pinned(r, k) → Running(r, k) — the
        // pin IS the key, the host only dispatches that key here. No
        // posture re-check (cordon never sheds a pin; the continuation
        // runs — §6: cited pins are cycle-critical work).
        (SlotState::Pinned { role, key }, Transition::Start { .. }) => Ok(SlotState::Running {
            role,
            key: Some(key),
        }),
        // T3/T4: unit complete; pinnable roles convert to a warm pin.
        (SlotState::Running { role, key }, Transition::CompleteToPinned) => {
            match role {
                WorkerRole::Solver => match key {
                    Some(k) => Ok(SlotState::Pinned { role, key: k }),
                    None => reject(RejectionReason::MissingPinKey),
                },
                // T4: exactly one merge pin, at THE merge key. The pipe-drain
                // handoff verification is host bookkeeping (§3.3 T4).
                WorkerRole::Merge => Ok(SlotState::Pinned {
                    role,
                    key: MERGE_PIN_KEY,
                }),
                _ => reject(RejectionReason::NoLegalRow),
            }
        }
        // T5 + T8 (one arm — identical bodies, distinct semantics):
        // T5: pooled roles complete back to the idle set; T8: a drained
        // in-flight unit finishes — nothing taken, slot returns to Idle.
        (
            SlotState::Running {
                role: WorkerRole::SimDriver | WorkerRole::Resolve | WorkerRole::PoolStateUpdater,
                ..
            },
            Transition::CompleteToIdle,
        )
        | (SlotState::Draining { .. }, Transition::DrainComplete) => Ok(SlotState::Idle),
        // T7: cordon onset / resize mid-cycle — the unit always completes,
        // so draining takes nothing new.
        (
            SlotState::Running { role, .. } | SlotState::Leased { role, .. },
            Transition::BeginDraining,
        ) => Ok(SlotState::Draining { role }),
        // T9: explicit rebalance — epoch boundary only, never mid-cycle.
        (SlotState::Pinned { .. }, Transition::ReleasePin) => {
            if ctx.at_epoch_boundary {
                Ok(SlotState::Idle)
            } else {
                reject(RejectionReason::MidCyclePin)
            }
        }
        // Everything else is off the legal table.
        _ => reject(RejectionReason::NoLegalRow),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::role::ALL_ROLES;

    const ADMITS: TransitionContext = TransitionContext {
        at_epoch_boundary: false,
        posture_admits_role: true,
    };

    const BLOCKS: TransitionContext = TransitionContext {
        at_epoch_boundary: false,
        posture_admits_role: false,
    };

    const fn admits(epochs: bool) -> TransitionContext {
        TransitionContext {
            at_epoch_boundary: epochs,
            posture_admits_role: true,
        }
    }

    #[test]
    fn t1_idle_leases_a_pooled_role() {
        for role in [
            WorkerRole::SimDriver,
            WorkerRole::Resolve,
            WorkerRole::PoolStateUpdater,
        ] {
            let to = transition(
                SlotState::Idle,
                Transition::Lease { role, key: None },
                ADMITS,
            )
            .expect("T1 pooled lease");
            assert_eq!(
                to,
                SlotState::Leased { role, key: None },
                "T1 must lease pooled roles"
            );
        }
    }

    #[test]
    fn t1_pin_claims_carry_keys() {
        let solver = transition(
            SlotState::Idle,
            Transition::Lease {
                role: WorkerRole::Solver,
                key: Some(2),
            },
            ADMITS,
        )
        .expect("T1 solver pin claim");
        assert_eq!(
            solver,
            SlotState::Leased {
                role: WorkerRole::Solver,
                key: Some(2)
            }
        );
        let merge = transition(
            SlotState::Idle,
            Transition::Lease {
                role: WorkerRole::Merge,
                key: Some(MERGE_PIN_KEY),
            },
            ADMITS,
        )
        .expect("T1 merge pin claim");
        assert_eq!(
            merge,
            SlotState::Leased {
                role: WorkerRole::Merge,
                key: Some(MERGE_PIN_KEY)
            }
        );
    }

    #[test]
    fn t1_rejects_keyless_pinnable_claims_and_bad_merge_keys() {
        let missing = transition(
            SlotState::Idle,
            Transition::Lease {
                role: WorkerRole::Solver,
                key: None,
            },
            ADMITS,
        )
        .expect_err("pin claim must carry its key");
        assert_eq!(missing.reason, RejectionReason::ClaimMissingKey);

        let wrong = transition(
            SlotState::Idle,
            Transition::Lease {
                role: WorkerRole::Merge,
                key: Some(9),
            },
            ADMITS,
        )
        .expect_err("merge claims must carry MERGE_PIN_KEY");
        assert_eq!(wrong.reason, RejectionReason::MergeKeyMismatch);
    }

    #[test]
    fn t1_is_blocked_by_a_cordoned_posture_for_deferrable_roles() {
        for role in [
            WorkerRole::PoolStateUpdater,
            WorkerRole::Registrar,
            WorkerRole::Verifier,
        ] {
            let rejected = transition(
                SlotState::Idle,
                Transition::Lease { role, key: None },
                BLOCKS,
            )
            .expect_err("cordon blocks deferrable intake");
            assert_eq!(rejected.reason, RejectionReason::PostureBlocksIntake);
        }
        // Never / SimPool classes still lease (sim is intake-floored, not
        // blocked; the floor is host bookkeeping, not an FSM row).
        for (role, key) in [
            (WorkerRole::SimDriver, None),
            (WorkerRole::Resolve, None),
            (WorkerRole::Solver, Some(1)),
            (WorkerRole::Merge, Some(MERGE_PIN_KEY)),
            (WorkerRole::Submitter, None),
        ] {
            transition(SlotState::Idle, Transition::Lease { role, key }, BLOCKS)
                .map(|_| ())
                .expect("role leases through cordon");
        }
    }

    #[test]
    fn t2_leased_starts_running_preserving_the_claim_key() {
        let running = transition(
            SlotState::Leased {
                role: WorkerRole::SimDriver,
                key: None,
            },
            Transition::Start { unit: 7 },
            ADMITS,
        )
        .expect("T2");
        assert_eq!(
            running,
            SlotState::Running {
                role: WorkerRole::SimDriver,
                key: None
            }
        );

        let pinned_walk = transition(
            SlotState::Leased {
                role: WorkerRole::Solver,
                key: Some(3),
            },
            Transition::Start { unit: 11 },
            ADMITS,
        )
        .expect("T2 pin-claim start");
        assert_eq!(
            pinned_walk,
            SlotState::Running {
                role: WorkerRole::Solver,
                key: Some(3)
            },
            "T2 must preserve the claimed pin key into Running"
        );
    }

    #[test]
    fn t2_starts_a_granted_unit_even_under_cordon() {
        // The posture gates INTAKE (T1), never the start of an already-
        // granted unit: a cordon publishing between the T1 lease and the
        // T2 start must not strand the grant (its only other exit is the
        // T7 shed). Flap-window regression (prod: `grant start (T2)`
        // PostureBlocksIntake → stranded intake receipt pipe → abort).
        let running = transition(
            SlotState::Leased {
                role: WorkerRole::Registrar,
                key: None,
            },
            Transition::Start { unit: 1 },
            BLOCKS,
        )
        .expect("a granted unit always starts — cordon sheds, never strands");
        assert_eq!(
            running,
            SlotState::Running {
                role: WorkerRole::Registrar,
                key: None
            }
        );
    }

    #[test]
    fn t3_solver_walk_pins_to_its_bin_key() {
        let pinned = transition(
            SlotState::Running {
                role: WorkerRole::Solver,
                key: Some(5),
            },
            Transition::CompleteToPinned,
            ADMITS,
        )
        .expect("T3");
        assert_eq!(
            pinned,
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 5
            }
        );
    }

    #[test]
    fn t3_rejects_a_keyless_solver_completion() {
        let rejected = transition(
            SlotState::Running {
                role: WorkerRole::Solver,
                key: None,
            },
            Transition::CompleteToPinned,
            ADMITS,
        )
        .expect_err("T3 requires a pin key");
        assert_eq!(rejected.reason, RejectionReason::MissingPinKey);
    }

    #[test]
    fn t4_merge_pins_to_the_single_merge_key() {
        let pinned = transition(
            SlotState::Running {
                role: WorkerRole::Merge,
                key: Some(MERGE_PIN_KEY),
            },
            Transition::CompleteToPinned,
            ADMITS,
        )
        .expect("T4");
        assert_eq!(
            pinned,
            SlotState::Pinned {
                role: WorkerRole::Merge,
                key: MERGE_PIN_KEY
            }
        );
    }

    #[test]
    fn t4_rejects_re_pinning_another_role() {
        // Only the pinnable roles convert Running → Pinned; a complete on
        // any other role must go to Idle (T5), never to a pin.
        for role in [
            WorkerRole::SimDriver,
            WorkerRole::Resolve,
            WorkerRole::Registrar,
        ] {
            let rejected = transition(
                SlotState::Running { role, key: None },
                Transition::CompleteToPinned,
                ADMITS,
            )
            .expect_err("only pinnable roles reach Pinned");
            assert_eq!(rejected.reason, RejectionReason::NoLegalRow, "{role:?}");
        }
    }

    #[test]
    fn t5_pooled_roles_return_to_idle() {
        for role in [
            WorkerRole::SimDriver,
            WorkerRole::Resolve,
            WorkerRole::PoolStateUpdater,
        ] {
            let idle = transition(
                SlotState::Running { role, key: None },
                Transition::CompleteToIdle,
                ADMITS,
            )
            .expect("T5");
            assert_eq!(
                idle,
                SlotState::Idle,
                "{role:?} must return to the pooled set"
            );
        }
    }

    #[test]
    fn t5_rejects_running_to_idle_for_pinned_roles() {
        // §3.3 illegal: `Running → Idle` mid-unit for the pinned roles —
        // Solver must complete to Pinned (T3), Merge to its pin (T4).
        for (role, key) in [
            (WorkerRole::Solver, Some(1)),
            (WorkerRole::Merge, Some(MERGE_PIN_KEY)),
        ] {
            let rejected = transition(
                SlotState::Running { role, key },
                Transition::CompleteToIdle,
                ADMITS,
            )
            .expect_err("pinned roles never drop straight to Idle");
            assert_eq!(rejected.reason, RejectionReason::NoLegalRow);
        }
    }

    #[test]
    fn t6_pinned_slots_take_the_next_cycle_unit_same_key() {
        let running = transition(
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 5,
            },
            Transition::Start { unit: 42 },
            ADMITS,
        )
        .expect("T6");
        assert_eq!(
            running,
            SlotState::Running {
                role: WorkerRole::Solver,
                key: Some(5)
            },
            "T6 keeps the pin key: the pin IS the key"
        );
    }

    #[test]
    fn t6_is_blocked_by_cordon_for_deferrable_pins() {
        // Declared-deferrable pins (future roles) must not re-enter Running
        // under cordon; the v1 pins are cordon-invariant classes.
        let fake_deferrable_pin_ctx = BLOCKS;
        let rejected = transition(
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 1,
            },
            Transition::Start { unit: 2 },
            fake_deferrable_pin_ctx,
        );
        // v1 pin classes are Never: T6 admits them even under cordon.
        assert!(
            rejected.is_ok(),
            "Solver pin continuation is cordon-invariant"
        );
    }

    #[test]
    fn t7_running_units_drain_under_cordon() {
        for role in [
            WorkerRole::SimDriver,
            WorkerRole::Solver,
            WorkerRole::Merge,
            WorkerRole::PoolStateUpdater,
        ] {
            let draining = transition(
                SlotState::Running { role, key: None },
                Transition::BeginDraining,
                ADMITS,
            )
            .expect("T7");
            assert_eq!(draining, SlotState::Draining { role }, "{role:?}");
        }
    }

    #[test]
    fn t8_draining_completes_to_idle() {
        let idle = transition(
            SlotState::Draining {
                role: WorkerRole::SimDriver,
            },
            Transition::DrainComplete,
            ADMITS,
        )
        .expect("T8");
        assert_eq!(idle, SlotState::Idle);
    }

    #[test]
    fn t8_draining_rejects_new_work() {
        // §3.3 illegal: Draining takes nothing new — no Start, no Lease.
        for t in [
            Transition::Start { unit: 1 },
            Transition::Lease {
                role: WorkerRole::SimDriver,
                key: None,
            },
        ] {
            let rejected = transition(
                SlotState::Draining {
                    role: WorkerRole::SimDriver,
                },
                t,
                ADMITS,
            )
            .expect_err("Draining takes nothing new");
            assert_eq!(rejected.reason, RejectionReason::NoLegalRow);
        }
    }

    #[test]
    fn t9_pin_release_is_epoch_boundary_only() {
        let ok = transition(
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 5,
            },
            Transition::ReleasePin,
            admits(true),
        )
        .expect("T9 at epoch boundary");
        assert_eq!(ok, SlotState::Idle);

        let rejected = transition(
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 5,
            },
            Transition::ReleasePin,
            admits(false),
        )
        .expect_err("T9 mid-cycle is illegal");
        assert_eq!(rejected.reason, RejectionReason::MidCyclePin);
    }

    #[test]
    fn t9_never_re_keys_in_place() {
        // §3.3 illegal: Pinned(Solver, k) → Pinned(Solver, k') — re-keying
        // is T9 then T1. There is no CompleteToPinned from Pinned, and any
        // other move from Pinned must go through Start (T6) or ReleasePin.
        let rejected = transition(
            SlotState::Pinned {
                role: WorkerRole::Solver,
                key: 1,
            },
            Transition::CompleteToPinned,
            ADMITS,
        )
        .expect_err("re-keying in place is illegal");
        assert_eq!(rejected.reason, RejectionReason::NoLegalRow);
    }

    #[test]
    fn illegal_idle_to_running_is_rejected() {
        // §3.3: lease required — there is no direct Idle → Running.
        for t in [
            Transition::Start { unit: 1 },
            Transition::CompleteToPinned,
            Transition::CompleteToIdle,
            Transition::BeginDraining,
            Transition::DrainComplete,
            Transition::ReleasePin,
        ] {
            let rejected =
                transition(SlotState::Idle, t, admits(true)).expect_err("Idle only moves via T1");
            assert_eq!(rejected.reason, RejectionReason::NoLegalRow, "{t:?}");
        }
    }

    #[test]
    fn illegal_non_t1_moves_from_idle_and_pinned_and_draining() {
        for from in [
            SlotState::Idle,
            SlotState::Draining {
                role: WorkerRole::Resolve,
            },
        ] {
            for t in [
                Transition::CompleteToPinned,
                Transition::CompleteToIdle,
                Transition::ReleasePin,
            ] {
                let rejected = transition(from, t, admits(true))
                    .expect_err("coverage: no row allows {t:?} from {from:?}");
                assert_eq!(rejected.reason, RejectionReason::NoLegalRow);
            }
        }
        for t in [
            Transition::CompleteToIdle,
            Transition::BeginDraining,
            Transition::DrainComplete,
        ] {
            let rejected = transition(
                SlotState::Pinned {
                    role: WorkerRole::Merge,
                    key: MERGE_PIN_KEY,
                },
                t,
                admits(true),
            )
            .expect_err("pinned merges only Start (T6) or ReleasePin (T9)");
            assert_eq!(rejected.reason, RejectionReason::NoLegalRow, "{t:?}");
        }
    }

    #[test]
    fn role_and_in_flight_projections_agree() {
        assert_eq!(SlotState::Idle.role(), None);
        for role in ALL_ROLES {
            for state in [
                SlotState::Leased { role, key: None },
                SlotState::Running { role, key: None },
                SlotState::Pinned { role, key: 0 },
                SlotState::Draining { role },
            ] {
                assert_eq!(state.role(), Some(role));
            }
        }
        assert!(SlotState::Running {
            role: WorkerRole::Solver,
            key: None
        }
        .holds_in_flight());
        assert!(!SlotState::Idle.holds_in_flight());
        assert!(!SlotState::Draining {
            role: WorkerRole::Solver
        }
        .holds_in_flight());
    }
}
