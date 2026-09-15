//! `FleetBudget` — the ONE budget authority bounding the SUM (design doc
//! §5; ADR-042 §3).
//!
//! Every consumer declares a peak-CPU share and a thread/slot count; the
//! fleet refuses to boot when the declared shares exceed the quota —
//! oversubscription is a configuration bug surfaced at boot, never a runtime
//! throttle storm. Fractional-quota policy (reviewed in the ADR): detection
//! keeps `degenbot_core::cpu_budget`'s ceil for worker-existence sizing,
//! while allocation arithmetic FLOORS — integer shares sum against
//! `floor(Q)` and the fractional remainder is spendable only by I/O-dominant
//! consumers (`SimDriver` slots, ambient I/O), whose measured duty is
//! partial-core by construction. `Q` comes from [`crate::quota`].
//!
//! # Discrepancy note (recorded, not redesigned)
//!
//! The design doc's worked 8-core table lists ambient `A = 2` while the
//! rule column reads `max(1, floor((Q−H)/4))`, which evaluates to 1 at
//! `Q = 8, H = 1` (leaving `S = 4`). This implementation follows the RULE
//! literally; a deployed operator recovers the table's `A = 2, S = 3` split
//! with `DEGENBOT_IO_WORKERS=2` (the terminal override both the table and
//! this code honor). The SUM invariant holds under either assignment.
//!
//! # Pin count = the LPT bin count (P6YXA6 sizing reconciliation)
//!
//! Solver pins are STRUCTURAL, not a share multiple: one seat per LPT bin,
//! the bin count following the same policy as
//! `degenbot_core::cpu_budget`'s solve bins — `floor(Q)` minus
//! [`degenbot_core::cpu_budget::DEFAULT_SOLVE_HEADROOM`]. At the deployed
//! Q = 8 that is 6 pins, exactly the bin count every dispatch arm binds at
//! (the ad-hoc `shares x 2` pin derivation — 8 seats at Q = 8 against
//! 6 bins — retires with the hard cutover). Walk ADMISSION stays the
//! share `S`: a gated bin parks, per design doc §5.
//!
//! # Cross-authority contract: allocation floors, detection ceils (TTANQJ)
//!
//! `degenbot_core::cpu_budget` CEILS fractional cgroup quotas for
//! worker-existence sizing (a 4.5-core quota still buys a 5th worker);
//! this authority FLOORS (`floor(Q) − headroom`), because Solver threads
//! are never I/O-dominant and must never spend the fractional remainder.
//! Consequence (property-tested in this file, `cross_authority`): the two
//! sizing authorities agree at integer quotas with `affinity >= Q`; under
//! a fractional quota with `affinity >= ceil(Q)` the legacy-stance solve
//! bins sit exactly one ABOVE the fleet seats. Integer quotas are the
//! deployment norm, and fleet-hosted cycles bind bins at the seat count
//! anyway, so the divergence is inert in production.

use degenbot_config::FleetConfig;

/// Fixed reserve share `H` (Python bridge, pump, `OTel`, async GC): the fleet
/// must never starve I/O.
pub const DEFAULT_RESERVE_CPUS: u64 = 1;
/// Minimum Solver share the fleet will host.
pub const MIN_SOLVER_CPUS: u64 = 2;
/// Today's `SimSlots` cap, preserved as the `SimDriver` slot cap (design doc §5).
pub const DEFAULT_SIM_SLOT_CAP: usize = 4;
/// The registration intake station's `PoolStateUpdater` slot cap (PRG-3,
/// ADR-042 F2: the registration crawl's pool-build consumers hosted as
/// keyed deferrable units). I/O-dominant by construction (RPC-bound
/// builds), so the billing follows the `SimDriver` model exactly.
pub const DEFAULT_POOL_STATE_UPDATER_SLOTS: usize = 4;

/// Terminal, typed overrides (config/env) consumed by [`FleetBudget::derive`]
/// — a configured value wins, is logged, and participates in the same sum
/// check (design doc §5: "overrides are terminal").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetOverrides {
    /// `fleet.reserve_cpus` — reserve share `H`.
    pub reserve_cpus: Option<u64>,
    /// `runtime.io_workers` — ambient I/O runtime `A`.
    pub ambient_io_workers: Option<u64>,
    /// `fleet.solver_cpus` — Solver share `S`.
    pub solver_cpus: Option<u64>,
    /// `fleet.sim_slot_cap` — `SimDriver` slot count.
    pub sim_slot_cap: Option<usize>,
    /// `fleet.pool_state_updater_slots` — `PoolStateUpdater` slot count.
    pub pool_state_updater_slots: Option<usize>,
    /// Solve headroom `H_s` in the seat formula `floor(Q) − H_s` (the
    /// documented floor-allocation constant; LW-T4 makes it an explicit
    /// boot-authority input).
    pub solve_headroom: Option<usize>,
}

impl BudgetOverrides {
    /// The typed-config projection (env vars land in the same fields via
    /// the loader's env layer).
    #[must_use]
    pub fn from_config(cfg: &degenbot_config::BotConfig) -> Self {
        Self {
            reserve_cpus: cfg.fleet.reserve_cpus.and_then(|v| u64::try_from(v).ok()),
            ambient_io_workers: cfg.runtime.io_workers.and_then(|v| u64::try_from(v).ok()),
            solver_cpus: cfg.fleet.solver_cpus.and_then(|v| u64::try_from(v).ok()),
            sim_slot_cap: cfg.fleet.sim_slot_cap,
            pool_state_updater_slots: cfg.fleet.pool_state_updater_slots,
            // The typed T8 follow-through (W6EMBF): the headroom override
            // reaches the budget derive; unset stays None and the documented
            // constant rules below.
            solve_headroom: cfg.solve.solve_headroom,
        }
    }
}

/// Why a budget was refused.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum BudgetError {
    /// Declared peak shares exceed the quota floor.
    #[error(
        "fleet budget oversubscribed: declared peak shares {declared} cores exceed \
         floor(quota {quota}) = {floor} — oversubscription is a configuration bug, failed at boot"
    )]
    Oversubscribed {
        /// The fractional quota (cores).
        quota: f64,
        /// `floor(quota)` the integer shares must sum to.
        floor: u64,
        /// The declared sum that exceeded it.
        declared: u64,
    },
    /// Fractional quota below the pinned-role floor (`H+A+R+M+2`).
    #[error(
        "fractional quota {quota:.2} below the pinned-role floor of {required} cores \
         (reserve H + ambient A + resolve R + merge M + the 2-core Solver minimum); \
         the fleet cannot host the pinned latency roles there — the \
         serial/sequential fallback DECISION POINT is here (reth \
         has_enough_parallelism(): lane capability is explicit, never silently \
         narrower); the fallback arm itself is a DOWNSTREAM decision (LW-T7)"
    )]
    QuotaTooSmallForPinnedRoles {
        /// The fractional quota (cores).
        quota: f64,
        /// The minimum usable quota.
        required: u64,
    },
    /// Solver share derives below the minimum.
    #[error("fleet Solver share derives to {solver} < the {min}-core minimum")]
    TooFewSolverCpus {
        /// The derived/declared Solver share.
        solver: u64,
        /// [`MIN_SOLVER_CPUS`].
        min: u64,
    },
    /// Fractional quota below the 2-core host floor (FLEETFLOOR FF-T2: one
    /// core for I/O work, one core for solve work) — no binding can host
    /// the fleet there, forced or not.
    #[error(
        "fractional quota {quota:.2} below the 2-core host floor (one core for I/O \
         work, one core for solve work) — no binding can host the fleet there \
         (raise the host CPU budget / affinity to at least 2 cores)"
    )]
    BelowHostFloor {
        /// The fractional quota (cores).
        quota: f64,
    },
    /// A `runtime.io_workers` override the resolved plan cannot honor
    /// (FF-T2): below the SMTH6M ambient floor (A >= 1) on the pinned
    /// binding, or off the serial binding\u0027s exactly-one ambient I/O lane.
    /// Refused with a hint, never a silent clamp.
    #[error(
        "runtime.io_workers override {requested} is out of bounds for the {binding} \
         binding (pinned: A >= 1, the SMTH6M ambient floor; serial: exactly one \
         ambient I/O lane) — fix or drop the override, never a silent clamp"
    )]
    IoWorkersOutOfBounds {
        /// The requested override.
        requested: u64,
        /// The binding the plan resolved (the label).
        binding: &'static str,
    },
}

impl BudgetError {
    /// The typed variant NAME (the closed, greppable refusal vocabulary).
    /// `runtime_status()`'s `tier_refused` string names the family the plan
    /// fell from (FF-T5 addendum, 452GZC); the Display wording stays the
    /// operator sentence with the quota, the floor, and the hint.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Oversubscribed { .. } => "Oversubscribed",
            Self::QuotaTooSmallForPinnedRoles { .. } => "QuotaTooSmallForPinnedRoles",
            Self::TooFewSolverCpus { .. } => "TooFewSolverCpus",
            Self::BelowHostFloor { .. } => "BelowHostFloor",
            Self::IoWorkersOutOfBounds { .. } => "IoWorkersOutOfBounds",
        }
    }
}

/// One consumer row of the boot allocation table (design doc §5) — declared
/// `(peak_cpus, thread_count)` per the ADR's sum-bounding contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsumerShare {
    /// Consumer name for the boot log.
    pub consumer: &'static str,
    /// Declared peak CPU share (cores).
    pub peak_cpus: u64,
    /// Declared thread/slot count (may exceed the share for I/O-dominant
    /// roles; the CPU share is what the authority bounds).
    pub thread_count: usize,
    /// Sizing rule in words (mirrors the worker-census `sizing` text).
    pub sizing: &'static str,
}

/// The derived fleet allocation — every consumer's declared share, the sum
/// check, and the fractional remainder policy (design doc §5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FleetBudget {
    /// The fractional quota `Q` this budget was derived from (cores).
    pub quota_cpus: f64,
    /// `floor(Q)` — integer shares sum against this.
    pub quota_floor: u64,
    /// Reserve `H` (Python bridge, pump, `OTel`, async GC). Fixed default 1.
    pub reserve_cpus: u64,
    /// Ambient I/O runtime `A` = `max(1, floor((Q−H)/4))`, terminal override
    /// `DEGENBOT_IO_WORKERS`.
    pub ambient_cpus: u64,
    /// Resolve `R` — fixed v1 (1 core, 12.4 ms/cycle measured).
    pub resolve_cpus: u64,
    /// Merge `M` — exactly one sidecar.
    pub merge_cpus: u64,
    /// Solver pins `S` = `floor(Q) − H − A − R − M` (or the terminal
    /// override); `>= 2` or the boot fails.
    pub solver_cpus: u64,
    /// Solver pin seats: STRUCTURAL — one per LPT bin
    /// (`floor(Q)` − `cpu_budget::DEFAULT_SOLVE_HEADROOM`; concurrent walk
    /// admission stays the share `S`).
    pub solver_pin_count: usize,
    /// `SimDriver` slots (duty-counted, spendable from the fractional
    /// remainder only), capped at today's `SimSlots` cap by default.
    pub sim_slot_cap: usize,
    /// `PoolStateUpdater` slots (duty-counted, spendable from the
    /// fractional remainder only) — the registration intake station's
    /// bounded per-role unit pool. Behind Solver precedence at dispatch.
    pub pool_state_updater_slots: usize,
    /// The fractional remainder `Q − Σ(shares)` — spendable ONLY by
    /// I/O-dominant consumers, enforced by construction: it is never
    /// included in the integer sum check.
    pub fractional_remainder: f64,
}

/// The per-tier projection mode (GAXX2Z): ONE owner
/// (`FleetBudget::project`) derives all three tier tables. The tier is MODE
/// DATA, not a second derivation — plan.rs's pinned/marked/serial arms
/// select a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetMode {
    /// The pinned-role derivation: integer shares sum-checked against
    /// `floor(Q)`, failing fast with the typed [`BudgetError`]s.
    Pinned,
    /// The forced-pinned-below-floor projection: pinned arithmetic with no
    /// sum enforcement (the plan's `oversubscribed` mark declares the
    /// deficit).
    PinnedMarked,
    /// The serial (2-5 core) projection: one ambient I/O lane, one cycle
    /// thread, exactly one solve seat.
    Serial,
}

/// The 2-core host floor: one core for I/O work, one core for solve work.
/// The plan's tier gate and the serial projection's logical thread count
/// share this ONE owner.
pub const HOST_FLOOR_CORES: u64 = 2;

impl FleetBudget {
    /// Derive the table for `quota_cpus` under the terminal overrides.
    /// Fails loudly (the typed [`BudgetError`]s) on over-subscription or a
    /// quota too small for the pinned latency roles.
    ///
    /// # Errors
    /// Any of the [`BudgetError`] fail-fast conditions.
    pub fn derive(quota_cpus: f64, overrides: &BudgetOverrides) -> Result<Self, BudgetError> {
        Self::project(quota_cpus, overrides, BudgetMode::Pinned)
    }

    /// The ONE tier projection (GAXX2Z): `mode` selects the pinned,
    /// forced-pinned-marked, or serial table, all built from the SAME shared
    /// formulas (H, A, R, M, the structural pin count, the slot caps). Only
    /// the pinned arm enforces the sum check; the other two are total by
    /// contract (the mark / the shared-thread topology carry the deficit).
    ///
    /// # Errors
    /// Any of the [`BudgetError`] fail-fast conditions — produced only by
    /// [`BudgetMode::Pinned`].
    #[expect(
        clippy::cast_possible_truncation,
        reason = "quota floors are small positive values (core counts)"
    )]
    #[expect(
        clippy::cast_precision_loss,
        reason = "core counts are exact in f64 at any realistic quota"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "the floor is clamped to >= 1.0 before the cast"
    )]
    pub(crate) fn project(
        quota_cpus: f64,
        overrides: &BudgetOverrides,
        mode: BudgetMode,
    ) -> Result<Self, BudgetError> {
        let quota_floor = quota_cpus.max(1.0).floor() as u64;

        // H — fixed default 1, terminal override.
        let reserve_cpus = overrides.reserve_cpus.unwrap_or(DEFAULT_RESERVE_CPUS);

        // A — the leftover-share rule, terminal DEGENBOT_IO_WORKERS override.
        let ambient_cpus = overrides
            .ambient_io_workers
            .unwrap_or_else(|| ((quota_floor.saturating_sub(reserve_cpus)) / 4).max(1));

        // R, M — fixed v1 consumers.
        let (resolve_cpus, merge_cpus) = (1, 1);
        let base = reserve_cpus + ambient_cpus + resolve_cpus + merge_cpus;

        let solve_headroom = overrides
            .solve_headroom
            .unwrap_or(degenbot_core::cpu_budget::DEFAULT_SOLVE_HEADROOM);
        let sim_slot_cap = overrides.sim_slot_cap.unwrap_or(DEFAULT_SIM_SLOT_CAP);
        let pool_state_updater_slots = overrides
            .pool_state_updater_slots
            .unwrap_or(DEFAULT_POOL_STATE_UPDATER_SLOTS);
        // Pin seats are STRUCTURAL (P6YXA6 reconciliation): one per LPT
        // bin, the bin count following cpu_budget's solve-bin POLICY —
        // minus the solve headroom, floored at 1 — but FLOORING the quota:
        // cpu_budget ceils fractional quotas for worker-existence; Solver
        // threads never spend the fractional remainder (§5).
        // Walk admission stays the share S — a gated bin parks (§5).
        let pinned_pin_count = usize::try_from(quota_floor)
            .unwrap_or(usize::MAX)
            .saturating_sub(solve_headroom)
            .max(1);

        match mode {
            BudgetMode::Pinned => {
                if base + MIN_SOLVER_CPUS > quota_floor {
                    return Err(BudgetError::QuotaTooSmallForPinnedRoles {
                        quota: quota_cpus,
                        required: base + MIN_SOLVER_CPUS,
                    });
                }
                // S — the leftover of the floor after the fixed consumers
                // (or the terminal override, checked against the same sum).
                let solver_cpus = overrides.solver_cpus.unwrap_or(quota_floor - base);
                if base + solver_cpus > quota_floor {
                    return Err(BudgetError::Oversubscribed {
                        quota: quota_cpus,
                        floor: quota_floor,
                        declared: base + solver_cpus,
                    });
                }
                if solver_cpus < MIN_SOLVER_CPUS {
                    return Err(BudgetError::TooFewSolverCpus {
                        solver: solver_cpus,
                        min: MIN_SOLVER_CPUS,
                    });
                }
                Ok(Self {
                    quota_cpus,
                    quota_floor,
                    reserve_cpus,
                    ambient_cpus,
                    resolve_cpus,
                    merge_cpus,
                    solver_cpus,
                    solver_pin_count: pinned_pin_count,
                    sim_slot_cap,
                    pool_state_updater_slots,
                    fractional_remainder: quota_cpus - (base + solver_cpus) as f64,
                })
            }
            BudgetMode::PinnedMarked => {
                let solver_cpus = overrides
                    .solver_cpus
                    .unwrap_or_else(|| quota_floor.saturating_sub(base).max(MIN_SOLVER_CPUS));
                Ok(Self {
                    quota_cpus,
                    quota_floor,
                    reserve_cpus,
                    ambient_cpus,
                    resolve_cpus,
                    merge_cpus,
                    solver_cpus,
                    solver_pin_count: pinned_pin_count,
                    sim_slot_cap,
                    pool_state_updater_slots,
                    // Oversubscribed: no spendable remainder exists (the
                    // deficit is the plan's mark, not a budget field) —
                    // clamp at zero.
                    fractional_remainder: (quota_cpus - (base + solver_cpus) as f64).max(0.0),
                })
            }
            BudgetMode::Serial => Ok(Self {
                quota_cpus,
                quota_floor,
                reserve_cpus,
                // Exactly one ambient I/O lane (validated by the plan).
                ambient_cpus: 1,
                resolve_cpus,
                merge_cpus,
                // The logical 2-core solve minimum: the serial cycle thread
                // runs solve work on the second core.
                solver_cpus: MIN_SOLVER_CPUS,
                // serial-0: exactly one solve seat (the FF-T4 contract).
                solver_pin_count: 1,
                sim_slot_cap,
                pool_state_updater_slots,
                // The binding owns two threads; everything above is spendable.
                fractional_remainder: (quota_cpus - HOST_FLOOR_CORES as f64).max(0.0),
            }),
        }
    }

    /// H + A + R + M + S — the declared sum the authority bounds.
    #[must_use]
    pub const fn declared_sum(&self) -> u64 {
        self.reserve_cpus
            + self.ambient_cpus
            + self.resolve_cpus
            + self.merge_cpus
            + self.solver_cpus
    }

    /// The boot allocation table (log/boot-dump consumer order).
    #[must_use]
    pub fn allocation_table(&self) -> Vec<ConsumerShare> {
        vec![
            ConsumerShare {
                consumer: "reserve",
                peak_cpus: self.reserve_cpus,
                thread_count: 0,
                sizing: "fixed; must never starve I/O",
            },
            ConsumerShare {
                consumer: "ambient_io",
                peak_cpus: self.ambient_cpus,
                thread_count: usize::try_from(self.ambient_cpus).unwrap_or(1),
                sizing: "max(1, floor((Q-H)/4)); DEGENBOT_IO_WORKERS override is terminal",
            },
            ConsumerShare {
                consumer: "resolve",
                peak_cpus: self.resolve_cpus,
                thread_count: usize::try_from(self.resolve_cpus).unwrap_or(1),
                sizing: "fixed v1 (12.4 ms/cycle measured)",
            },
            ConsumerShare {
                consumer: "merge",
                peak_cpus: self.merge_cpus,
                thread_count: usize::try_from(self.merge_cpus).unwrap_or(1),
                sizing: "exactly one sidecar",
            },
            ConsumerShare {
                consumer: "solver",
                peak_cpus: self.solver_cpus,
                thread_count: self.solver_pin_count,
                sizing: "Q - H - A - R - M (>= 2 or fail-fast); pins = one per LPT bin (floor(Q) - solve headroom)",
            },
        ]
    }

    /// Re-declare the shares under a changed quota (posture/logged event).
    /// Pure re-derivation; the HOST decides which pins change (T9) and never
    /// re-keys mid-cycle.
    ///
    /// # Errors
    /// Any of the [`BudgetError`] fail-fast conditions under the new quota.
    pub fn resize(
        &self,
        new_quota_cpus: f64,
        new_overrides: &BudgetOverrides,
    ) -> Result<Self, BudgetError> {
        Self::derive(new_quota_cpus, new_overrides)
    }

    /// Whether moving from this budget to `next` requires pin re-keying
    /// (a pin-count change) — the epoch-boundary rebalance trigger (T9).
    #[must_use]
    pub const fn pins_require_rekey(&self, next: &FleetBudget) -> bool {
        self.solver_pin_count != next.solver_pin_count
    }
}

/// Fractional quota detection seam: the typed `fleet.quota_cpus` override
/// wins (terminal, per the §5 override rule); unset detects the real cgroup
/// via [`crate::quota::fractional_cpu_budget`]. Tests inject values
/// directly into [`FleetBudget::derive`].
#[must_use]
pub fn detected_quota_cpus(cfg: &FleetConfig) -> f64 {
    cfg.quota_cpus
        .filter(|q| *q >= 1.0)
        .unwrap_or_else(crate::quota::fractional_cpu_budget)
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    fn overrides() -> BudgetOverrides {
        BudgetOverrides::default()
    }

    #[test]
    fn the_8_core_quota_table_sums_exactly_against_the_floor() {
        let b = FleetBudget::derive(8.0, &overrides()).expect("8-core quota hostable");
        assert_eq!(b.quota_floor, 8);
        // SUM invariant: H + A + R + M + S = Q (design doc §5).
        assert_eq!(b.declared_sum(), b.quota_floor);
        assert_eq!(b.reserve_cpus, 1);
        assert_eq!(b.resolve_cpus, 1);
        assert_eq!(b.merge_cpus, 1);
        // The rule column: A = max(1, floor((Q-H)/4)) = 1, leaving S = 4.
        // (The doc TABLE's A=2/S=3 split is reachable via the terminal
        // DEGENBOT_IO_WORKERS override — see the module discrepancy note.)
        assert_eq!(b.ambient_cpus, 1);
        assert_eq!(b.solver_cpus, 4);
        // Pins are STRUCTURAL: one seat per LPT bin = floor(Q) - the solve
        // headroom (allocation FLOORS the quota; cpu_budget's worker-
        // existence detection ceils it — the TTANQJ property in this file
        // pins that split) — not the 2:1 parked-wait over-subscription
        // (P6YXA6 sizing note).
        assert_eq!(b.solver_pin_count, 6);
    }

    #[test]
    fn the_worked_8_core_table_split_is_reachable_via_the_terminal_override() {
        // DEGENBOT_IO_WORKERS=2 (the current deployment) reproduces the
        // doc §5 worked table exactly: A=2, S=3, sum 8.
        let b = FleetBudget::derive(
            8.0,
            &BudgetOverrides {
                ambient_io_workers: Some(2),
                ..overrides()
            },
        )
        .expect("hostable");
        assert_eq!(b.ambient_cpus, 2);
        assert_eq!(b.solver_cpus, 3);
        assert_eq!(b.solver_pin_count, 6);
        assert_eq!(b.declared_sum(), 8);
    }

    #[test]
    fn a_quota_below_the_pinned_role_floor_fails_fast() {
        // 4.5-core quota: floor is 4 but H+A+R+M+2 = 6 > 4 — the two pinned
        // latency roles cannot be hosted there (the integer sum floors; the
        // fractional remainder is never part of the sum check).
        let err = FleetBudget::derive(4.5, &overrides()).expect_err("too small");
        assert!(matches!(
            err,
            BudgetError::QuotaTooSmallForPinnedRoles { .. }
        ));
    }

    /// FF-T5 addendum (452GZC): the typed refusal family carries a closed,
    /// greppable NAME vocabulary — the runtime status names the family it
    /// fell from (never a free-text parse); the wording stays the operator
    /// sentence.
    #[test]
    fn the_refusal_family_names_its_typed_variants() {
        let cases: [(BudgetError, &str); 5] = [
            (
                BudgetError::Oversubscribed {
                    quota: 4.0,
                    floor: 4,
                    declared: 7,
                },
                "Oversubscribed",
            ),
            (
                BudgetError::QuotaTooSmallForPinnedRoles {
                    quota: 4.0,
                    required: 6,
                },
                "QuotaTooSmallForPinnedRoles",
            ),
            (
                BudgetError::TooFewSolverCpus {
                    solver: 1,
                    min: MIN_SOLVER_CPUS,
                },
                "TooFewSolverCpus",
            ),
            (BudgetError::BelowHostFloor { quota: 1.5 }, "BelowHostFloor"),
            (
                BudgetError::IoWorkersOutOfBounds {
                    requested: 0,
                    binding: "pinned",
                },
                "IoWorkersOutOfBounds",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.name(), expected, "{expected} names itself");
            // The Display sentence never doubles as the family name — the
            // tier_refused string composes them ("Name: message").
            assert!(
                !err.to_string().contains(expected),
                "the wording stays the sentence; the name rides explicitly: {err}"
            );
        }
    }

    #[test]
    fn fractional_quota_banks_the_remainder_outside_the_integer_sum() {
        // 6.5-core quota: floor 6, base (H1+A1+R1+M1) = 4, S = 2; the 0.5
        // remainder banks outside the sum check (I/O-dominant spend only).
        let b = FleetBudget::derive(6.5, &overrides()).expect("hostable");
        assert_eq!(b.quota_floor, 6);
        assert_eq!(b.declared_sum(), 6);
        assert!((b.fractional_remainder - 0.5).abs() < 1e-9);
        assert_eq!(b.solver_cpus, 2);
        assert_eq!(b.solver_pin_count, 4);
    }

    // ---- LW-T4 (Seam A+G1): boot authority — quota-derived sizing, no ambient fallback

    /// The documented floor-allocation formula is the SOLE seat authority:
    /// `pins == floor(Q) − solve_headroom` (floored at 1) with the shares
    /// and the fractional remainder derived from the SAME quota — over the
    /// matrix quota × headroom, under INJECTED quotas only (no detector may
    /// run in tests): the test passes host-hardware-independently (cgroup-
    /// limited CI and a bare host alike). A quota below the capacity floor
    /// refuses TYPED, naming the floor and the serial/sequential fallback
    /// decision point — never a silently narrower lane.
    #[test]
    fn seat_count_is_quota_headroom_derived_under_the_documented_formula_only() {
        for quota in [1.0_f64, 1.5, 4.0, 8.0, 24.0] {
            for headroom in [1_usize, 2] {
                let overrides = BudgetOverrides {
                    solve_headroom: Some(headroom),
                    ..overrides()
                };
                // Test-quotas are positive reals (1.0..24.0): floor() is exact
                // and the cast cannot lose sign or magnitude here.
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "quota floors are exact for the injected positive reals"
                )]
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "the injected quotas are all strictly positive"
                )]
                let q_floor = quota.max(1.0).floor() as u64;
                // Boot OR refuse, never a silent mis-size: the pinned-role
                // floor (H+A+R+M+2) and the headroom+1 floor both refuse
                // TYPED, naming the floor and the serial fallback point.
                let b = match FleetBudget::derive(quota, &overrides) {
                    Ok(b) => b,
                    Err(err @ BudgetError::QuotaTooSmallForPinnedRoles { .. }) => {
                        let msg = err.to_string();
                        assert!(
                            msg.contains(&format!("{q_floor} numerically"))
                                || msg.contains("pinned-role floor"),
                            "the capacity-floor error must name the floor: {msg}"
                        );
                        assert!(
                            msg.contains("serial/sequential"),
                            "the typed floor error must name the serial/sequential fallback decision point (reth has_enough_parallelism): {msg}"
                        );
                        continue;
                    }
                    Err(other) => {
                        // Test-fixture tripwire: an unexpected refusal here is
                        // the failure — panic IS the assertion.
                        #[expect(
                            clippy::panic,
                            reason = "an unexpected typed refusal is the fixture's failure mode"
                        )]
                        {
                            panic!("unexpected typed refusal at quota {quota}: {other}")
                        }
                    }
                };
                assert!(
                    q_floor > u64::try_from(headroom).unwrap_or(0),
                    "a quota below headroom+1 must have taken the typed floor arm above"
                );
                assert_eq!(
                    b.solver_pin_count as u64,
                    q_floor
                        .saturating_sub(u64::try_from(headroom).unwrap_or(0))
                        .max(1),
                    "seat count must be f(quota, headroom): quota {quota}, headroom {headroom}"
                );
                assert_eq!(
                    b.declared_sum(),
                    q_floor,
                    "the integer share sum must be exactly floor(Q)"
                );
                // Exact-by-construction: the injected quotas are binary-exact
                // (1.0/1.5/4.0/8.0/24.0) and floor(Q) is an integer, so the
                // subtraction loses nothing — a strict comparison is exact.
                {
                    // The fills are an exact small integer; q_floor < 2^24 for
                    // every injected quota, so the u32 conversion cannot lose.
                    let q = u32::try_from(q_floor).expect("injected quotas are < 2^24");
                    let quoted = quota - f64::from(q);
                    assert!(
                        (b.fractional_remainder - quoted).abs() < 1e-12,
                        "the fractional remainder is Q − floor(Q), spendable only by I/O:                          {} vs {quoted}",
                        b.fractional_remainder
                    );
                }
            }
        }
    }

    /// LW-T4 (reth research lesson-4): the boot authority never falls back to
    /// ambient host state — `available_parallelism` must not appear in the
    /// sizing paths (the DETECTION ceilings live in degenbot-core / quota.rs;
    /// the seat authority floors purely from the injected quota). This is a
    /// DELIBERATE regular-expression tripwire mirroring the pyo3-free
    /// dependency assertion — replace it with a behavioral assertion if
    /// budget.rs ever sizes off a parameterizable source instead of the
    /// injected quota.
    #[test]
    fn seat_sizing_never_reads_ambient_parallelism() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/budget.rs"))
            .expect("crate source readable");
        // Scan ONLY the lib code: the tests module legitimately names the
        // identifier (this assertion's own text).
        let lib = src
            .split("#[cfg(test)]")
            .next()
            .expect("the lib segment always exists");
        assert!(
            !lib.contains("available_parallelism"),
            "the boot authority must never size lanes off ambient parallelism (reth lesson-4)"
        );
    }

    /// LW-T4: oversubscription is TYPED at boot (derive returns the error —
    /// `abort_executor` is reserved for RUNTIME strand abandonment only)
    /// and the message names BOTH numbers (declared sum AND quota floor).
    #[test]
    fn oversubscription_is_typed_at_boot_and_names_both_numbers() {
        let err = FleetBudget::derive(
            8.0,
            &BudgetOverrides {
                ambient_io_workers: Some(2),
                solver_cpus: Some(4),
                ..overrides()
            },
        )
        .expect_err("H1+A2+R1+M1+S4 = 9 > 8");
        assert!(matches!(err, BudgetError::Oversubscribed { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains('9'),
            "the message must name the declared sum: {msg}"
        );
        assert!(
            msg.contains('8'),
            "the message must name the quota floor: {msg}"
        );
    }

    /// LW-T4: quota below the capacity floor is a TYPED refusal naming the
    /// floor and pointing at the serial/sequential fallback decision point
    /// (reth `has_enough_parallelism()` lesson: lane capability is explicit,
    /// never silently narrower). The fallback arm itself is LW-T7's.
    #[test]
    fn quota_below_the_capacity_floor_refuses_typed_naming_the_fallback_point() {
        let err = FleetBudget::derive(2.0, &overrides())
            .expect_err("quota 2.0 < the pinned-role floor must refuse typed");
        assert!(matches!(
            err,
            BudgetError::QuotaTooSmallForPinnedRoles { .. }
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("serial"),
            "the typed floor error must name the serial/sequential fallback decision point: {msg}"
        );
    }

    #[test]
    fn oversubscription_by_overrides_fails_at_boot() {
        let err = FleetBudget::derive(
            8.0,
            &BudgetOverrides {
                ambient_io_workers: Some(2),
                solver_cpus: Some(4),
                ..overrides()
            },
        )
        .expect_err("H1+A2+R1+M1+S4 = 9 > 8");
        assert!(matches!(err, BudgetError::Oversubscribed { .. }));
    }

    #[test]
    fn a_sub_minimum_solver_share_is_refused() {
        let err = FleetBudget::derive(
            8.0,
            &BudgetOverrides {
                solver_cpus: Some(1),
                ..overrides()
            },
        )
        .expect_err("S=1 < the 2-core minimum");
        assert!(matches!(err, BudgetError::TooFewSolverCpus { .. }));
    }

    #[test]
    fn resize_redeclares_shares_and_keeps_the_sum_invariant() {
        let big = FleetBudget::derive(8.0, &overrides()).expect("8");
        let small = big.resize(6.5, &overrides()).expect("6.5 hostable");
        assert_eq!(small.declared_sum(), small.quota_floor);
        // Pin-count change flags the T9 rebalance (never a mid-cycle re-key).
        assert!(big.pins_require_rekey(&small));
        // Idempotent re-declaration under an unchanged quota.
        let same = big.resize(8.0, &overrides()).expect("8 again");
        assert!(!big.pins_require_rekey(&same));
    }

    #[test]
    fn the_registration_intake_station_is_duty_counted_like_sim() {
        // PoolStateUpdater slots default to the ADR-042 F2 station size and
        // stay OUTSIDE the declared integer sum (I/O-dominant billing —
        // exactly the SimDriver model).
        let b = FleetBudget::derive(8.0, &overrides()).expect("hostable");
        assert_eq!(b.pool_state_updater_slots, DEFAULT_POOL_STATE_UPDATER_SLOTS);
        assert_eq!(b.declared_sum(), b.quota_floor);
        // The terminal override wins (same terminal rule as the rest).
        let b2 = FleetBudget::derive(
            8.0,
            &BudgetOverrides {
                pool_state_updater_slots: Some(6),
                ..overrides()
            },
        )
        .expect("hostable");
        assert_eq!(b2.pool_state_updater_slots, 6);
        assert_eq!(b2.declared_sum(), b2.quota_floor);
    }

    #[test]
    fn the_pool_state_updater_override_projects_from_the_typed_config() {
        let mut cfg = degenbot_config::BotConfig::default();
        cfg.fleet.pool_state_updater_slots = Some(6);
        let o = BudgetOverrides::from_config(&cfg);
        assert_eq!(o.pool_state_updater_slots, Some(6));
        let b = FleetBudget::derive(8.0, &o).expect("hostable");
        assert_eq!(b.pool_state_updater_slots, 6);
    }

    #[test]
    fn allocation_table_covers_every_consumer_row() {
        let b = FleetBudget::derive(8.0, &overrides()).expect("8");
        let consumers: Vec<&str> = b
            .allocation_table()
            .into_iter()
            .map(|s| s.consumer)
            .collect();
        for want in ["reserve", "ambient_io", "resolve", "merge", "solver"] {
            assert!(consumers.contains(&want), "missing {want}");
        }
    }

    #[test]
    fn typed_config_projection_carries_the_override_fields() {
        let mut cfg = degenbot_config::BotConfig::default();
        cfg.fleet.solver_cpus = Some(3);
        cfg.runtime.io_workers = Some(2);
        cfg.fleet.sim_slot_cap = Some(6);
        let o = BudgetOverrides::from_config(&cfg);
        assert_eq!(o.solver_cpus, Some(3));
        assert_eq!(o.ambient_io_workers, Some(2));
        assert_eq!(o.sim_slot_cap, Some(6));
        let b = FleetBudget::derive(8.0, &o).expect("hostable");
        assert_eq!(b.solver_cpus, 3);
        assert_eq!(b.sim_slot_cap, 6);
    }

    /// W6EMBF: the `solve.solve_headroom` typed key reaches the budget
    /// derive end-to-end — through the LOADER route (not just a hand-built
    /// struct) — while the default stays `None` (the documented constant
    /// rules below).
    #[test]
    fn the_solve_headroom_override_projects_from_the_typed_config() {
        let loaded = degenbot_config::BotConfigLoader::new()
            .without_env()
            .with_cli("solve.solve_headroom", "2")
            .load()
            .expect("the headroom override must load from the explicit CLI layer");
        assert_eq!(loaded.config.solve.solve_headroom, Some(2));
        let o = BudgetOverrides::from_config(&loaded.config);
        assert_eq!(o.solve_headroom, Some(2));
        let b = FleetBudget::derive(8.0, &o).expect("hostable");
        // The documented formula: pins = floor(Q) - H_s (8 - 2 = 6).
        assert_eq!(b.solver_pin_count, 6);
        assert_eq!(b.declared_sum(), b.quota_floor);
        // The default config projects None — the documented constant rules.
        assert_eq!(
            BudgetOverrides::from_config(&degenbot_config::BotConfig::default()).solve_headroom,
            None
        );
    }

    /// GAXX2Z helper: the shared ambient formula A = max(1, (floor(Q)-H)/4).
    fn ambient_formula(floor: u64, reserve: u64) -> u64 {
        ((floor.saturating_sub(reserve)) / 4).max(1)
    }

    /// GAXX2Z helper: the structural pin formula floor(Q) - headroom, >= 1.
    fn pin_formula(floor: u64, headroom: usize) -> usize {
        usize::try_from(floor)
            .unwrap_or(usize::MAX)
            .saturating_sub(headroom)
            .max(1)
    }

    /// GAXX2Z helper: the pinned arm's sum-checked invariants.
    #[expect(
        clippy::cast_precision_loss,
        reason = "test quotas and small share sums are exact in f64"
    )]
    fn assert_pinned_projection(
        quota: f64,
        floor: u64,
        ov: &BudgetOverrides,
        reserve: u64,
        ambient: u64,
        pins: usize,
    ) {
        let base = reserve + ambient + 2;
        match FleetBudget::project(quota, ov, BudgetMode::Pinned) {
            Ok(b) => {
                assert_eq!(b.reserve_cpus, reserve, "pinned reserve H");
                assert_eq!(
                    b.ambient_cpus, ambient,
                    "pinned ambient A = max(1, (Q-H)/4)"
                );
                assert_eq!(
                    b.solver_cpus,
                    ov.solver_cpus.unwrap_or(floor - base),
                    "pinned solver S = floor(Q) - H - A - R - M"
                );
                assert_eq!(
                    b.solver_pin_count, pins,
                    "pinned pins = floor(Q) - headroom"
                );
                assert!(
                    b.declared_sum() <= floor,
                    "pinned sum-check never oversubscribes floor(quota)"
                );
                if ov.solver_cpus.is_none() {
                    assert_eq!(b.declared_sum(), floor, "pinned default fills floor(quota)");
                }
                assert!(
                    (b.fractional_remainder - (quota - b.declared_sum() as f64)).abs() < 1e-9,
                    "pinned remainder banks Q - declared_sum"
                );
            }
            Err(refusal) => assert!(
                matches!(
                    refusal,
                    BudgetError::QuotaTooSmallForPinnedRoles { .. }
                        | BudgetError::Oversubscribed { .. }
                        | BudgetError::TooFewSolverCpus { .. }
                ),
                "pinned refusal is a typed budget error, got {refusal:?}"
            ),
        }
    }

    /// GAXX2Z helper: the marked arm's total pinned arithmetic.
    fn assert_marked_projection(
        quota: f64,
        floor: u64,
        ov: &BudgetOverrides,
        reserve: u64,
        ambient: u64,
        pins: usize,
    ) {
        let base = reserve + ambient + 2;
        let b = FleetBudget::project(quota, ov, BudgetMode::PinnedMarked).expect("marked total");
        assert_eq!(b.reserve_cpus, reserve, "marked reserve H");
        assert_eq!(
            b.ambient_cpus, ambient,
            "marked ambient A = max(1, (Q-H)/4)"
        );
        assert_eq!(
            b.solver_cpus,
            ov.solver_cpus
                .unwrap_or_else(|| floor.saturating_sub(base).max(MIN_SOLVER_CPUS)),
            "marked solver S = max(floor(Q) - base, MIN)"
        );
        assert_eq!(
            b.solver_pin_count, pins,
            "marked pins = floor(Q) - headroom"
        );
        assert!(
            b.fractional_remainder >= 0.0,
            "marked remainder clamps at zero"
        );
    }

    /// GAXX2Z helper: the serial arm's one-lane / one-seat topology.
    #[expect(
        clippy::cast_precision_loss,
        reason = "the host floor is a tiny core count, exact in f64"
    )]
    fn assert_serial_projection(quota: f64, ov: &BudgetOverrides, reserve: u64) {
        let b = FleetBudget::project(quota, ov, BudgetMode::Serial).expect("serial total");
        assert_eq!(b.reserve_cpus, reserve, "serial reserve H");
        assert_eq!(
            b.ambient_cpus, 1,
            "serial owns exactly one ambient I/O lane"
        );
        assert_eq!(
            b.solver_cpus, MIN_SOLVER_CPUS,
            "serial logical 2-core solve"
        );
        assert_eq!(b.solver_pin_count, 1, "serial-0: exactly one solve seat");
        assert!(
            (b.fractional_remainder - (quota - HOST_FLOOR_CORES as f64).max(0.0)).abs() < 1e-9,
            "serial remainder is Q - the 2-core host floor"
        );
    }

    /// GAXX2Z: ONE projection owner. All three tier projections are produced
    /// by `FleetBudget::project(mode)` from the SAME shared formulas; this
    /// pin walks representative quotas and override shapes and asserts each
    /// mode's invariants against the derive formulas (the sum-check vs
    /// floor(quota), the reserve/ambient/solver/pin relationships) — not
    /// against the function's own output.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "quota floors are small positive values (core counts)"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "the test quota is floored at 1.0 before the cast"
    )]
    fn every_projection_mode_shares_the_derive_formulas() {
        let cases: [BudgetOverrides; 3] = [
            BudgetOverrides::default(),
            BudgetOverrides {
                ambient_io_workers: Some(2),
                ..BudgetOverrides::default()
            },
            BudgetOverrides {
                reserve_cpus: Some(2),
                solver_cpus: Some(3),
                ..BudgetOverrides::default()
            },
        ];
        for quota in [2.0_f64, 2.5, 4.0, 5.99, 6.0, 6.5, 8.0, 24.0, 33.25] {
            let floor = quota.max(1.0).floor() as u64;
            for ov in &cases {
                let reserve = ov.reserve_cpus.unwrap_or(DEFAULT_RESERVE_CPUS);
                let ambient = ov
                    .ambient_io_workers
                    .unwrap_or_else(|| ambient_formula(floor, reserve));
                let headroom = ov
                    .solve_headroom
                    .unwrap_or(degenbot_core::cpu_budget::DEFAULT_SOLVE_HEADROOM);
                let pins = pin_formula(floor, headroom);
                assert_pinned_projection(quota, floor, ov, reserve, ambient, pins);
                assert_marked_projection(quota, floor, ov, reserve, ambient, pins);
                assert_serial_projection(quota, ov, reserve);
            }
        }
    }

    mod cross_authority {
        //! TTANQJ : the settled contract between the two CPU
        //! sizing authorities over ARBITRARY quota shapes. Fleet allocation
        //! floors: `seats = max(1, floor(Q) - headroom)`. `cpu_budget`
        //! worker-existence ceils: `solve = max(1, min(ceil(cgroup Q),
        //! affinity) - headroom)`. Consequences (see the module-note
        //! addendum above): agreement iff the quota is integer AND affinity
        //! covers it; a fractional quota under affinity >= ceil(Q) leaves
        //! legacy-stance solve bins exactly ONE above the fleet seats.
        use super::*;
        use proptest::prelude::*;

        prop_compose! {
            fn quota_shape()(base in 1u64..=512u64, half in 0u8..2u8, affinity in 1u64..=1024u64)
                -> (u64, u8, u64) {
                    (base, half, affinity)
                }
        }

        proptest! {
            #![proptest_config(proptest::test_runner::Config::with_cases(512))]
            #[test]
            #[expect(
                clippy::cast_precision_loss,
                reason = "quota units (1e6 scale, <= 5e8) are exact in f64"
            )]
            fn pins_and_solve_workers_follow_the_documented_floor_ceil_split(
                (base, half, affinity) in quota_shape(),
            ) {
                // cgroup encodes the quota at 1_000_000-unit periods; a
                // half-step quota is a fractional f64 core count.
                let units = base * 1_000_000 + u64::from(half) * 500_000;
                let floor_q = units / 1_000_000;
                let fractional = units % 1_000_000 != 0;
                // effective_budget_from_with_roots: ceil the cgroup quota,
                // then min with affinity (the budget floored at 1).
                let budget = units.div_ceil(1_000_000).min(affinity).max(1);
                let budget_usize = usize::try_from(budget).unwrap_or(usize::MAX);
                let solve = degenbot_core::cpu_budget::solve_worker_count_from(
                    None, None, budget_usize,
                );
                let q = (units as f64) / 1_000_000.0;

                if floor_q < 6 {
                    // Below the pinned-role floor (H+A+R+M+2) nothing hosts.
                    // (bound first: prop_assert stringifies its expression,
                    // and `{ .. }` from `matches!` would break the format
                    // string)
                    let refused = matches!(
                        FleetBudget::derive(q, &BudgetOverrides::default()),
                        Err(BudgetError::QuotaTooSmallForPinnedRoles { .. })
                    );
                    prop_assert!(refused);
                } else {
                    // derive fails only on over-subscription or a
                    // too-small quota; our defaults cannot oversubscribe
                    // (floor_q >= 6 => base + MIN_SOLVER_CPUS <= floor).
                    let Ok(b) = FleetBudget::derive(q, &BudgetOverrides::default()) else {
                        return Err(TestCaseError::fail(format!(
                            "hostable shape refused: q = {q}"
                        )));
                    };
                    // The P6YXA6 formula, as written.
                    prop_assert_eq!(
                        b.solver_pin_count,
                        usize::try_from((floor_q - 2).max(1)).unwrap_or(usize::MAX)
                    );
                    if !fractional && affinity >= floor_q {
                        // Integer quota, adequate affinity: agreement.
                        prop_assert_eq!(b.solver_pin_count, solve);
                    } else if fractional && budget == floor_q + 1 {
                        // Fractional quota under affinity >= ceil(Q):
                        // worker-existence buys exactly one more core than
                        // allocation spends; pins never bank the remainder.
                        prop_assert_eq!(solve, b.solver_pin_count + 1);
                    } else if !fractional && affinity < floor_q {
                        // Affinity-take-over: the (smaller) budget rules
                        // the solve bins; the seat count never shrinks.
                        prop_assert!(solve <= b.solver_pin_count);
                    } else if fractional && budget <= floor_q {
                        // Affinity clamped below the ceiling.
                        prop_assert!(solve <= b.solver_pin_count);
                    }
                    let _ = fractional; // documented above
                }
            }
        }
    }

    mod derivation {
        //! CVURM7 : `derive` over ARBITRARY quotas AND
        //! overrides. Total over the input space: a typed `Ok` holding the
        //! invariants (integer shares sum to at most the floor; the
        //! fractional remainder banks exactly `Q - declared_sum`; seats
        //! follow the structural formula) or a typed `BudgetError`
        //! classifying the refusal. Never a panic, never a silently mis-sized
        //! budget. NOTE: with a terminal `solver_cpus` override BELOW the
        //! recorded leftover the banked remainder legitimately exceeds 1
        //! core (the sum check bounds over-subscription only; under-declared
        //! shares bank for I/O-dominant spend) — so no `< 1` bound here.
        use super::*;
        use proptest::prelude::*;

        fn overrides_shape() -> impl Strategy<Value = BudgetOverrides> {
            (
                1u64..=64u64,
                0u64..=16u64,
                0u64..=64u64,
                0usize..=16usize,
                0usize..=16usize,
            )
                .prop_map(|(h, a, s, sim, psu)| BudgetOverrides {
                    reserve_cpus: Some(h),
                    ambient_io_workers: Some(a),
                    solver_cpus: Some(s),
                    sim_slot_cap: Some(sim),
                    pool_state_updater_slots: Some(psu),
                    solve_headroom: None,
                })
        }

        proptest! {
            #![proptest_config(proptest::test_runner::Config::with_cases(512))]
            #[test]
            #[expect(
                clippy::cast_precision_loss,
                reason = "quota units (1e6 scale) and share sums (<= 640) are exact in f64"
            )]
            fn derive_is_total_and_typed_over_the_whole_input_space(
                quota_units in 1u64..=64_000_000u64,
                ov in overrides_shape(),
            ) {
                let q = (quota_units as f64) / 1_000_000.0;
                let floor_q = quota_units / 1_000_000;
                match FleetBudget::derive(q, &ov) {
                    Ok(b) => {
                        // Integer shares sum against (never over) the floor.
                        prop_assert!(b.declared_sum() <= b.quota_floor);
                        prop_assert_eq!(b.quota_floor, floor_q);
                        // The fractional remainder is banked exactly,
                        // never negative and never beyond the quota.
                        let sum_f = b.declared_sum() as f64;
                        prop_assert!((b.fractional_remainder - (q - sum_f)).abs() < 1e-9);
                        prop_assert!(b.fractional_remainder >= 0.0 && b.fractional_remainder <= q);
                        // Seats are structural: overrides never move them.
                        prop_assert_eq!(
                            b.solver_pin_count,
                            usize::try_from(floor_q).unwrap_or(usize::MAX)
                                .saturating_sub(degenbot_core::cpu_budget::DEFAULT_SOLVE_HEADROOM)
                                .max(1)
                        );
                    }
                    Err(
                        BudgetError::QuotaTooSmallForPinnedRoles { .. }
                        | BudgetError::Oversubscribed { .. }
                        | BudgetError::TooFewSolverCpus { .. }
                        // FF-T2 (MEBF4V): the plan-tier refusal classes.
                        // The derive itself never produces them (the plan
                        // does); listed so the match stays exhaustive and
                        // a future arm is still a compile error.
                        | BudgetError::BelowHostFloor { .. }
                        | BudgetError::IoWorkersOutOfBounds { .. },
                    ) => {
                        // Exhaustive typed refusal classes; the match arms
                        // above make any NEW error variant a compile error.
                    }
                }
            }
        }
    }
}
