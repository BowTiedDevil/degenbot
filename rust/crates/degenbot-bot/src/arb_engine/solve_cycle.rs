//! The solve cycle as a deep module (ADR-045, ergo task `E7V2S6`).
//!
//! ## Scope (this file)
//!
//! **T1** pins the data-type seam of ADR-045: the `CycleOutcome` /
//! `CycleArm` / `ResolveCensus` / `Registration` vocabulary, with
//! `CycleArm::label()` reproducing the ADR-043 `cycle.arm` labels
//! `"shed" | "skipped_empty" | "detached"` byte-for-byte. Those types
//! remain un-consumed until the T4/T5 wiring.
//!
//! **T3** (`ANVHXW`) assembles [`SolveCycle`] — the field group ONLY: the
//! resolve companions, the cycle-transient stash, the solve output, the
//! admissions stance, the detached-arm collaborator, the walk/projection
//! recorders, and the seven `for_test` knobs. The engine holds it as
//! `engine.cycle`; the pinned `*_for_test` engine setters delegate into it.
//! No method body moves yet — the `draw` / `run_epoch` / `register_path` /
//! `register_and_solve_path` / `solve_all_paths` / `merge_detached_item` /
//! `forget` interface arrives at T4.
//!
//! ## Test ledger — which tests flip at which task
//!
//! * **Green today** (they pin the ADR-043 vocabulary and the target type
//!   shape): `cycle_arm_labels_are_byte_stable`,
//!   `cycle_arm_is_dispatch_selects_detached_arms`,
//!   `cycle_outcome_exposes_block_and_arm_label`,
//!   `registration_encodes_created_and_resolved`,
//!   `pending_new_path_result_survives_one_dirty_cycle`.
//! * **Loud-on-fix inverted pin**: `dedup_hit_does_not_touch_pending_new_carry`.
//!   It PASSES while the defect stands (bare-u64 `register_path` makes a dedup
//!   hit indistinguishable from a fresh register, so `register_and_solve_path`
//!   re-arms the pending-new-paths carry), and FAILS LOUDLY when T4/T5 gates
//!   the carry write on `Registration.created`. The first green run at T4/T5
//!   is to REMOVE the `#[should_panic]` and flip the assertion to the positive
//!   contract.
//! * **Ignored until T4/T5** (the target surface does not exist yet, kept as a
//!   compilable sketch so the interlock is on record):
//!   `run_epoch_consumes_the_epoch_work_carried_delta` (the candidate-8
//!   wiring test — the cycle consumes the epoch's work-carried delta, one
//!   owner, no second swappable handle) and
//!   `register_and_solve_path_reports_registration_created`.
//!
//! The module-level `dead_code` expectation keeps the non-test build compiling
//! while the T1 types remain un-consumed (the T4/T5 wiring removes it).
#![cfg_attr(not(test), expect(dead_code))]

use std::sync::Arc;

use dashmap::DashMap;
use hashbrown::{HashMap, HashSet};

use ::degenbot_solvers::mixed::{ResolvedMixedPath, SolvePathResult};

use super::block_cursor::BlockCursor;
use super::detached_cycle::DetachedCycle;
use super::inline_sim::SimulatedPathResult;
use super::path_lifecycle::PathSolveStatus;
use super::DeferredReRecordHook;
use crate::bot_core::resolve::HopProjectionCache;

/// The solve cycle's owned state (ADR-045, ergo task `ANVHXW`).
///
/// T3 scope: the field group ONLY — the resolve companions, the
/// cycle-transient stash, the solve output, the admissions stance, the
/// detached-arm collaborator, the walk/projection recorders, and the seven
/// `for_test` knobs. The behavior methods (`draw` / `run_epoch` /
/// `register_path` / ...) arrive at T4; no method body moves here.
///
/// The engine holds this as `engine.cycle`; the white-box tests re-index
/// mechanically to `engine.cycle.*` and the pinned `*_for_test` engine
/// setters delegate into these fields (ADR-041: knob names never change at
/// the call site).
#[expect(clippy::struct_excessive_bools)] // construction stances (projection memo, admission, streaming, resolve-par) + the draw verdict — distinct stances, not flag soup
pub(crate) struct SolveCycle {
    // --- Resolve companions -------------------------------------------
    /// Resolved path states (mutated on each solve). Entries are Arc-shared
    /// into the parallel solve dispatch (f701ccd3 staging fix) — immutable
    /// between resolve passes, so staging is refcount bumps, not deep clones
    /// of the CL tick-range sequences.
    pub(crate) path_resolved: HashMap<u64, Arc<ResolvedMixedPath>>,
    /// Path solve-eligibility state machine (R522XA): per registered path the
    /// [`PathSolveStatus`] that decides whether a dirty-pool fan-out must
    /// (re)resolve it. Replaces the scattered `valid` bool + ad-hoc skip rules.
    pub(crate) path_status: HashMap<u64, PathSolveStatus>,
    /// Per-path snapshot of every hop's `pool_update_block` at the last
    /// successful resolve. A byte-identical snapshot on the next cycle means
    /// the whole solve intake (all hop states) is unchanged (epic RZRORC last
    /// leaf).
    pub(crate) resolved_update_snapshot: HashMap<u64, Vec<u64>>,
    /// Hop-projection memo (pool,direction) -> snapshot@nonce. Shared across
    /// all resolve call sites so a dirty pool's tick walk runs once per state
    /// change and serves every referencing path from the cache.
    pub(crate) hop_projection_cache: HopProjectionCache,
    /// Monotonic count of actual family projections (cache misses). Test-
    /// observable; emitted on the solve-phase resolve event.
    pub(crate) hop_projection_count: u64,
    /// Fused hop-projection memo switch (KGXFT7 winner promotion): resolved
    /// ONCE at construction.
    pub(crate) cl_projection_memo: bool,
    /// Telemetry string cache: path id -> formatted hop description, built
    /// once at first emission (paths are immutable after registration).
    pub(crate) path_description_cache: parking_lot::Mutex<HashMap<u64, Arc<str>>>,

    // --- Cycle-transient stash ----------------------------------------
    /// 6XB6NJ: the ONE engine block cursor — the consolidated owner of the
    /// block-coordinate residue. Every advance rule lives on the cursor.
    pub(crate) cursor: BlockCursor,
    /// QTZGFL: the DRAW's consumption verdict for the cycle currently between
    /// `on_resolve` and the dispatch — `true` when the admission draw's
    /// budget was zero (the cycle SHEDS).
    pub(crate) admission_draw_zero: bool,
    /// Cold-start trace: the CURRENT solve cycle's dispatch arm —
    /// `detached` | `skipped_empty` | `shed` (the cycle-span vocabulary).
    pub(crate) cycle_arm: &'static str,
    /// REMED1 T2: which entry drove the CURRENT solve cycle - `drain`
    /// (`EngineStages::solve_dirty`, per-log streaming) vs `finalize`.
    pub(crate) solve_entry: &'static str,
    /// Paths registered via `register_and_solve_path` that have been eagerly
    /// solved and appended to `results`, awaiting the next dirty-cycle merge.
    pub(crate) pending_new_paths: HashSet<u64>,
    /// Reuse-eligibility counter for the current solve cycle (probe only;
    /// reset each `solve_dirty` and surfaced on the resolve event).
    pub(crate) paths_same_state_this_cycle: u64,

    // --- Solve output -------------------------------------------------
    /// Last solved results, keyed by path ID for O(1) updates.
    ///
    /// RAYPAR engine-shard T1 (C42WKO): sharded into a `DashMap` so Python
    /// `latest_results` reads never park behind the drain-lock held engine
    /// `Mutex`.
    pub(crate) results: DashMap<u64, SolvePathResult>,
    /// SIMPIPE2 T3: the per-path inline payloads resolved in the solve
    /// workers (SIMPIPE2 T2's off-lock seam). Keyed by path id.
    pub(crate) inline_payloads: DashMap<u64, SimulatedPathResult>,

    // --- Admissions stance --------------------------------------------
    /// QTZGFL: construction-time admission stance (`DEGENBOT_SOLVE_ADMISSION`,
    /// default OFF for the experiment).
    pub(crate) solve_admission: bool,
    /// QTZGFL: the un-merged-result pipe depth target in KEYS, clamped at
    /// construction to `1..=detached_cycle::DETACHED_INFLIGHT_CAP`.
    pub(crate) admission_target_depth: u64,
    /// QTZGFL: the retained (carried) key retention window W in blocks.
    pub(crate) admission_retention_blocks: u64,

    // --- Collaborators / recorders ------------------------------------
    /// THE one solve-arm machine (P37YJG): the per-cycle states, the merge
    /// pipe, the gauge pair, the seq counters, the ledger door, and the
    /// disposition counters. See [`DetachedCycle`].
    pub(crate) detached_cycle: DetachedCycle,
    /// The engine-owned cross-block walk-composition memo (SU7MAE T3, Q12a):
    /// passed into the solve entries by handle; epoch advances at the
    /// block-lifecycle start.
    pub(crate) walk_memo: Arc<::degenbot_solvers::mobius_v3_int::WalkMemo>,
    /// Per-path previous-block MEASURED walk sims (recorded by `solve_fn`
    /// after each solve; lock-free-read at bin construction). Refines the
    /// LPT makespan predictor for stable pool shapes (loop-12 KUKHMX).
    pub(crate) last_walk_sims: Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// Per-path previous-block MEASURED gate time (µs, recorded by `solve_fn`;
    /// lock-free-read at bin construction). Loop-18.
    pub(crate) last_gate_us: Arc<parking_lot::Mutex<HashMap<u64, u64>>>,
    /// T3 (epic BXUSGL): emit each clamp-passed above-threshold result as an
    /// IMMEDIATE single-entry [`ResultBatch`] during the drain instead of
    /// waiting for the pump debounce. Construction-time stance.
    pub(crate) streaming_delivery: bool,
    /// KJWIK5: the deferred-path re-record hook (the ledger carry for
    /// `paths.deferred_future_price` deferrals).
    pub(crate) deferred_re_record: Option<DeferredReRecordHook>,
    /// KAHU5W: the chunked-parallel resolve stance as an instance value.
    pub(crate) resolve_par_stance: bool,

    // --- for_test knobs (ADR-041: names pinned on the engine) ---------
    /// Test-only: hook invoked at the start of each path solve.
    #[cfg(test)]
    pub(crate) test_solve_delay: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// 43E3H3 red-first: test-only per-path PANIC hook.
    #[cfg(test)]
    pub(crate) test_solve_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// KJWIK5 test seam: force the future-price deferral for these path ids.
    #[cfg(test)]
    pub(crate) test_force_deferred: Option<HashSet<u64>>,
    /// Test-only: the drain appends each merged path id here.
    #[cfg(test)]
    pub(crate) merge_probe: Option<Arc<parking_lot::Mutex<Vec<u64>>>>,
    /// AQV6EF red-first: test-only per-path PANIC hook for the MERGE seat.
    #[cfg(test)]
    pub(crate) test_merge_panic: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// WFF6MM test harness: when ON (default), a DIRECT
    /// `rebuild_and_solve_affected` / `solve_dirty` call merges its own
    /// just-enqueued detached pipe INLINE.
    #[cfg(test)]
    pub(crate) test_sync_merge: bool,
    /// WFF6MM test harness: the inline drain's cached handle on the merge pipe.
    #[cfg(test)]
    pub(crate) test_merge_rx:
        Option<std::sync::mpsc::Receiver<crate::arb_engine::executor::LaneOutcome>>,
}

/// The per-cycle resolve census — the submission/resolve counts the stage
/// hooks and telemetry read off a [`CycleOutcome`] (ADR-045).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolveCensus {
    /// Affected keys the cycle drew (the epoch's work-carried delta size).
    pub(crate) affected: u64,
    /// Paths that (re)resolved to a fresh state this cycle.
    pub(crate) resolved: u64,
    /// Paths whose hop-state snapshot was byte-identical to the stored one.
    pub(crate) same_state: u64,
    /// Actual family projections performed (cache misses) this cycle.
    pub(crate) projections: u64,
}

/// The typed cycle arm (ADR-045) — replaces the engine's string `cycle_arm`
/// stash. [`CycleArm::label`] reproduces the ADR-043 `cycle.arm` vocabulary
/// byte-for-byte; [`CycleArm::is_dispatch`] says whether the arm reached the
/// detached solve dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CycleArm {
    /// Draw-time zero budget: nothing submitted, no `begin_cycle`, no seq
    /// tick; the cursor still advances and the keys stay in the ledger for a
    /// later cycle (carry).
    Shed {
        /// Keys left in the ledger by the zero-budget draw.
        keys_affected: usize,
        /// Outstanding detached solves at the shed verdict.
        in_flight: u64,
        /// The admission target depth at the shed verdict.
        target_depth: u64,
    },
    /// No affected paths (and no pending registration carry): a
    /// bookkeeping-only pass that advances the cursor.
    SkippedEmpty {
        /// The (empty) keys-affected count, kept for symmetry.
        keys_affected: usize,
    },
    /// The detached solve arm actually enqueued work.
    Solved {
        /// The detached-cycle sequence number (`begin_cycle`).
        seq: u64,
        /// LPT bin count the enqueue used.
        bins: usize,
        /// Paths staged for solve.
        staged: usize,
        /// Paths skipped as invalid at staging.
        invalid: usize,
        /// Paths deferred because their price clock ran ahead of the anchor.
        deferred_future_price: usize,
    },
    /// The detached arm ran but dissolved to no solve work (every staged path
    /// was invalid or deferred) — still a dispatch cycle.
    Dissolved {
        /// Paths skipped as invalid at staging.
        invalid: usize,
        /// Paths deferred because their price clock ran ahead of the anchor.
        deferred_future_price: usize,
    },
}

impl CycleArm {
    /// The ADR-043 `cycle.arm` label, byte-for-byte.
    #[must_use]
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Shed { .. } => "shed",
            Self::SkippedEmpty { .. } => "skipped_empty",
            Self::Solved { .. } | Self::Dissolved { .. } => "detached",
        }
    }

    /// Whether this arm reached the detached solve dispatch. The shed and
    /// skipped-empty arms are non-dispatch (no `begin_cycle`, no submission);
    /// both detached arms are.
    #[must_use]
    pub(crate) const fn is_dispatch(&self) -> bool {
        matches!(self, Self::Solved { .. } | Self::Dissolved { .. })
    }
}

/// The typed fact a solve cycle returns (ADR-045): the solved-block
/// coordinate, the resolve census, and the typed arm. Stage hooks read this
/// instead of poking the engine's string stash.
#[derive(Debug, Clone)]
pub(crate) struct CycleOutcome {
    /// The block the cycle anchored to (the monotone cursor's solved block).
    pub(crate) solved_block: u64,
    /// The cycle's resolve census.
    pub(crate) census: ResolveCensus,
    /// The typed cycle arm.
    pub(crate) arm: CycleArm,
}

impl CycleOutcome {
    /// The solved-block coordinate.
    #[must_use]
    pub(crate) const fn solved_block(&self) -> u64 {
        self.solved_block
    }

    /// The ADR-043 arm label (delegates to [`CycleArm::label`]).
    #[must_use]
    pub(crate) const fn arm_label(&self) -> &'static str {
        self.arm.label()
    }
}

/// The typed registration result (ADR-045) — a dedup hit and a fresh register
/// are typable facts, not `pending_new_paths` timing.
#[derive(Debug, Clone)]
pub(crate) struct Registration {
    /// The registered (or dedup-matched) path id.
    pub(crate) path_id: u64,
    /// `true` for a fresh registration; `false` for a dedup hit.
    pub(crate) created: bool,
    /// The freshly-resolved snapshot for a fresh registration; `None` on a
    /// dedup hit (the existing snapshot is reused).
    pub(crate) resolved: Option<Arc<ResolvedMixedPath>>,
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
    use ::degenbot_solvers::mixed::{HopType, PoolHop};
    use alloy::primitives::{aliases::U112, Address, U256};
    use hashbrown::HashSet;

    fn usdc(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }

    fn weth(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    /// Two divergent V2 pools plus the profitable two-hop path between them —
    /// the same fixture `register_and_solve_path_eagerly_solves` uses.
    fn divergent_v2_path(engine: &ArbitrageEngine) -> Vec<PoolHop> {
        let a = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let b = engine.register_v2_pool(
            Address::from([0x12u8; 20]),
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        vec![
            PoolHop {
                pool_id: a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: b,
                zero_for_one: true,
            },
        ]
    }

    // ---------------------------------------------------------------------
    // Green-pin: the ADR-043 vocabulary + the ADR-045 type seam.
    // ---------------------------------------------------------------------

    #[test]
    fn cycle_arm_labels_are_byte_stable() {
        assert_eq!(
            CycleArm::Shed {
                keys_affected: 3,
                in_flight: 8,
                target_depth: 8,
            }
            .label(),
            "shed"
        );
        assert_eq!(
            CycleArm::SkippedEmpty { keys_affected: 0 }.label(),
            "skipped_empty"
        );
        assert_eq!(
            CycleArm::Solved {
                seq: 1,
                bins: 4,
                staged: 10,
                invalid: 0,
                deferred_future_price: 0,
            }
            .label(),
            "detached"
        );
        assert_eq!(
            CycleArm::Dissolved {
                invalid: 2,
                deferred_future_price: 1,
            }
            .label(),
            "detached"
        );
    }

    #[test]
    fn cycle_arm_is_dispatch_selects_detached_arms() {
        assert!(!CycleArm::Shed {
            keys_affected: 3,
            in_flight: 8,
            target_depth: 8,
        }
        .is_dispatch());
        assert!(!CycleArm::SkippedEmpty { keys_affected: 0 }.is_dispatch());
        assert!(CycleArm::Solved {
            seq: 1,
            bins: 4,
            staged: 10,
            invalid: 0,
            deferred_future_price: 0,
        }
        .is_dispatch());
        assert!(CycleArm::Dissolved {
            invalid: 2,
            deferred_future_price: 1,
        }
        .is_dispatch());
    }

    #[test]
    fn cycle_outcome_exposes_block_and_arm_label() {
        let outcome = CycleOutcome {
            solved_block: 21_000_000,
            census: ResolveCensus {
                affected: 12,
                resolved: 11,
                same_state: 4,
                projections: 7,
            },
            arm: CycleArm::Solved {
                seq: 9,
                bins: 4,
                staged: 12,
                invalid: 1,
                deferred_future_price: 0,
            },
        };
        assert_eq!(outcome.solved_block(), 21_000_000);
        assert_eq!(outcome.arm_label(), "detached");
        assert_eq!(outcome.census.affected, 12);
        assert!(outcome.arm.is_dispatch());
    }

    #[test]
    fn registration_encodes_created_and_resolved() {
        let fresh = Registration {
            path_id: 7,
            created: true,
            resolved: Some(Arc::new(ResolvedMixedPath::default())),
        };
        assert!(fresh.created);
        assert!(fresh.resolved.is_some());

        let dedup = Registration {
            path_id: 7,
            created: false,
            resolved: None,
        };
        assert!(!dedup.created);
        assert_eq!(dedup.path_id, 7);
        assert!(dedup.resolved.is_none());
    }

    /// Pin today's carry contract: an eager registration survives exactly one
    /// following dirty cycle (merge, not discard).
    #[test]
    fn pending_new_path_result_survives_one_dirty_cycle() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);
        let path_id = engine
            .register_and_solve_path(hops)
            .expect("eager register must solve the divergent path");

        assert!(
            engine.cycle.pending_new_paths.contains(&path_id),
            "register_and_solve_path arms the pending-new carry"
        );

        let empty = crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        engine.rebuild_and_solve_affected(&empty, 1, &BlockMetadata::default());

        assert!(
            engine.cycle.pending_new_paths.is_empty(),
            "the dirty cycle consumes and clears the carry"
        );
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        assert!(
            results.contains_key(&path_id),
            "the eager result merges across the cycle instead of being discarded"
        );
    }

    // ---------------------------------------------------------------------
    // Loud-on-fix inverted pin: the dedup fact + the pending-new carry.
    // ---------------------------------------------------------------------

    /// INVERTED PIN. The target contract (ADR-045): a dedup hit yields
    /// `Registration { created: false, .. }` and must NOT touch the
    /// pending-new carry. Today `register_path` returns a bare `u64`, so the
    /// dedup hit re-enters `register_and_solve_path`'s eager-solve branch and
    /// re-arms the carry — this assertion fails until T4/T5 gate the carry
    /// write on `created`. The pin passes while the defect stands; it inverts
    /// at T4/T5, so the first green run there is to REMOVE the `#[should_panic]`
    /// and flip the assertion to the positive (`engine.cycle.pending_new_paths.is_empty()`).
    #[test]
    #[should_panic(expected = "RED at T1 (flips at T4/T5)")]
    fn dedup_hit_does_not_touch_pending_new_carry() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);

        let path_id = engine
            .register_and_solve_path(hops.clone())
            .expect("fresh register");
        assert!(
            engine.cycle.pending_new_paths.contains(&path_id),
            "a fresh register arms the carry"
        );

        // Model the carry consumed by one dirty cycle.
        engine.cycle.pending_new_paths.clear();

        let again = engine
            .register_and_solve_path(hops)
            .expect("dedup register");
        assert_eq!(again, path_id, "a dedup hit retains the registry id");

        assert!(
            engine.cycle.pending_new_paths.is_empty(),
            "RED at T1 (flips at T4/T5): a dedup hit (created=false) must not touch the pending-new carry; today register_and_solve_path re-arms it because the bare u64 cannot express created"
        );
    }

    // ---------------------------------------------------------------------
    // Ignored until T4/T5: the target surface does not exist yet.
    // ---------------------------------------------------------------------

    /// Sketch of the candidate-8 interlock, gated to the `run_epoch` wiring
    /// test at T4. The assertion that lands then: the cycle consumes the
    /// epoch's work-carried delta (the `affected` vector drawn by the Resolved
    /// stage) and does not hold or re-read a second swappable handle (the
    /// retired `admission_draw_zero` stash).
    #[test]
    #[ignore = "gates green at T4/T5: SolveCycle::run_epoch wiring test (candidate-8 interlock)"]
    fn run_epoch_consumes_the_epoch_work_carried_delta() {
        use crate::bot_core::EpochDelta;

        // The epoch ledger is the SINGLE work-carried owner. The Resolved
        // stage draws the cycle's `affected` keys from it; the Solved stage's
        // `SolveCycle::run_epoch` must consume exactly that vector.
        let delta = EpochDelta::new(11u64);
        delta.record_affected(HopType::V2, 1, 11);
        delta.record_affected(HopType::V2, 2, 11);
        let affected = delta.draw_freshest(usize::MAX);
        assert_eq!(affected.len(), 2, "the draw is the cycle's affected set");
        assert!(
            delta.is_empty(),
            "the draw IS the single consumption decision — no second handle sees the keys"
        );

        // T4 wiring (unrepresentable at T1 — `SolveCycle::run_epoch` does not
        // exist yet):
        //   let outcome = engine.cycle.run_epoch(entry, affected, block, &metadata, &registry);
        //   assert_eq!(outcome.census.affected, 2);
        //   assert!(outcome.arm.is_dispatch());
    }

    /// Sketch of the fresh-vs-dedup `Registration` facts, gated to T4/T5.
    #[test]
    #[ignore = "gates green at T4/T5: register_and_solve_path returns Registration{created}"]
    fn register_and_solve_path_reports_registration_created() {
        let mut engine = ArbitrageEngine::new();
        let hops = divergent_v2_path(&engine);

        let fresh = engine
            .register_and_solve_path(hops.clone())
            .expect("fresh register");
        let dedup = engine
            .register_and_solve_path(hops)
            .expect("dedup register");

        // T4/T5 target: `fresh { created: true, resolved: Some(_) }` and
        // `dedup { created: false, resolved: None }`, both carrying the same
        // `path_id`. Not representable at T1 (a bare `u64`).
        assert_eq!(fresh, dedup);
    }
}
