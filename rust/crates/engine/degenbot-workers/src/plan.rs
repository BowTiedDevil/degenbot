//! `plan` — the `FleetPlan` tiered boot authority (FLEETFLOOR FF-T2 / MEBF4V).
//!
//! LW-T4 made the budget the sole sizing authority with ONE
//! floor: below the pinned-role floor the boot refused. FF-T2 generalizes
//! that one floor into ordered HOST TIERS, one pure function of the
//! budget:
//!
//! - pinned: the pinned-role derivation succeeds (floor(Q) >= H+A+R+M+2)
//!   — today's topology, byte-stable (the derivation is the ONE
//!   `FleetBudget::derive`, unchanged);
//! - serial: 2-5 core hosts (the pinned derivation refuses but the host
//!   has the 2-core minimum: one core for I/O work, one core for solve
//!   work) — the arm itself lands with FF-T4; until then the boot
//!   refuses with the tier's own typed refusal, never a silent narrow
//!   (the reth `has_enough_parallelism` lesson: lane capability is
//!   explicit);
//! - refused: below 2 cores — `BudgetError::BelowHostFloor`, typed.
//!
//! Forced bindings (`runtime.fleet_profile = pinned | serial`, env
//! `DEGENBOT_FLEET_PROFILE`) run on any host with 2 or more cores; a forced
//! pinned binding below the floor is MARKED `oversubscribed` (latency
//! contract void, correctness contract intact) — loud, never silent.
//!
//! # Composition, never duplication
//!
//! The plan picks the BINDING and the per-binding budget projection;
//! `SlotLayout::of` stays the ONE geometry derivation under each
//! binding (a serial-plan projection must yield a legal non-empty layout:
//! 1 solver seat, 1 resolve, >= 1 poolupd, sim per budget). The plan is
//! boot-frozen like the landed frozen-layout property: no resize
//! re-derivation is ever triggered. The plan INHERITS budget.rs's
//! documented derive-vs-doc-table discrepancy (the module note): the rule
//! column is the authority; a fix, if ever wanted, is its own card.
//!
//! # Plan identity
//!
//! `PLAN_ID` names and versions the algebra (`fleetplan/1`): the ONE boot
//! log line names it with the binding and the detected budget, and a
//! tier edit bumps the version so logs stay unambiguous.

use degenbot_config::FleetProfile;

use crate::budget::{BudgetError, BudgetMode, BudgetOverrides, FleetBudget};
use crate::dispatcher::BootError;

/// The plan algebra's name + version (named and versioned by contract: the
/// boot log line names it; a tier edit bumps the version).
pub const PLAN_ID: &str = "fleetplan/1";

/// The minimum usable host (the epic's goal state): one core for I/O
/// work, one core for solve work. Below this the plan refuses TYPED.
/// Canonically owned by budget.rs — re-exported for the plan's
/// tier gate.
pub use crate::budget::HOST_FLOOR_CORES;

/// The fleet host binding (the adapter that maps lanes to threads; the
/// FLEETFLOOR design contract). The census/log label is
/// [`Binding::label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// The pinned binding: dedicated seats per role, today's topology
    /// (6 cores or more under default overrides).
    Pinned,
    /// The serial binding: one ambient I/O lane plus one cycle lane, one
    /// solve seat (the arm lands with FF-T4).
    Serial,
}

impl Binding {
    /// The census `binding` label (the field's closed vocabulary:
    /// pinned / shared / logical). The fleet roles stamp the PINNED
    /// binding today; the serial binding maps them onto shared threads as
    /// LOGICAL lanes when it lands (FF-T4) — the label changes with the
    /// binding, the row does not.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Serial => "logical",
        }
    }

    /// The binding NAME (error-message vocabulary: pinned / serial — the
    /// plan profile the operator set, distinct from the census label).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Serial => "serial",
        }
    }
}

impl std::fmt::Display for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The NAME (pinned / serial) — the log/error vocabulary; the
        // census label is the thread-mapping vocabulary ([`Binding::label`]).
        f.write_str(self.name())
    }
}

/// The resolved boot plan: one pure function of the budget (the tier
/// decision + the per-binding projection inputs). Boot-frozen by design;
/// [`Clone`] so the host can carry it without borrowing.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetPlan {
    /// The plan identity ([`PLAN_ID`]) — the boot log line names it.
    pub id: &'static str,
    /// The binding the host tier resolves to.
    pub binding: Binding,
    /// A forced pinned binding below the pinned-role floor: the latency
    /// contract is void (the declared shares exceed the quota), the
    /// correctness contract is intact. Loud, never silent.
    pub oversubscribed: bool,
    /// The detected budget the plan decided on (cores, fractional).
    pub budget_cpus: f64,
    /// The typed budget refusal the resolved plan overrode or fell from
    /// (`None` when nothing was refused): under `auto`, the pinned-budget
    /// refusal that placed this host in the serial tier (the boot re-raises
    /// it while the serial arm is pending — FF-T4 — so the refusal stays
    /// the typed, named budget error the pinned derivation produced); under
    /// a FORCED pinned profile below the pinned-role floor, the overridden
    /// `QuotaTooSmallForPinnedRoles` the marked plan runs past (the runtime
    /// status names the floor it fell from — FF-T5 addendum, 452GZC);
    /// `None` for forced serial and every unrefused boot.
    pub tier_refusal: Option<BudgetError>,
}

impl FleetPlan {
    /// The per-binding budget projection: the [`FleetBudget`] the binding
    /// boots under. Every binding selects a [`BudgetMode`] and the ONE
    /// owner ([`FleetBudget::project`]) produces the table — this
    /// module holds no budget arithmetic. The PINNED eligible path is the
    /// sum-checked derivation (byte-stable); the marked and serial modes
    /// are total (the sum invariant is void there by contract —
    /// oversubscribed / shared threads).
    ///
    /// # Errors
    /// [`BootError`] — the pinned eligible projection's typed refusal. The
    /// marked/serial projections are total; their layout legality is
    /// `SlotLayout::of`'s dead-station check, asserted by the
    /// plan-tier invariant tests.
    pub fn projected_budget(&self, overrides: &BudgetOverrides) -> Result<FleetBudget, BootError> {
        let mode = match self.binding {
            Binding::Pinned if !self.oversubscribed => BudgetMode::Pinned,
            Binding::Pinned => BudgetMode::PinnedMarked,
            Binding::Serial => BudgetMode::Serial,
        };
        FleetBudget::project(self.budget_cpus, overrides, mode).map_err(BootError::from)
    }

    /// The typed refusal the BOOT raises while the serial arm is pending
    /// (FF-T4): under `auto` the tier's own pinned-budget refusal (the
    /// CI-stable message); under a FORCED serial profile the named
    /// pending-arm invariant (the operator asked for a binding that does
    /// not exist yet — refusing is the honest answer, never a silent
    /// narrow).
    #[must_use]
    pub fn pending_serial_refusal(&self) -> BootError {
        match self.tier_refusal {
            Some(refusal) => BootError::Budget(refusal),
            None => BootError::Invariant(
                "the serial binding (the 2-5-core tier) lands with FF-T4 \
                 — forced serial refuses rather than run the pinned topology \
                 silently narrower",
            ),
        }
    }
}

/// ONE pure function of the budget (the tiered boot authority). Total,
/// deterministic, no host reads: the caller supplies the detected
/// quota, the profile, and the terminal overrides. The budget stays the
/// sole sizing authority (min(cgroup quota, affinity), fractional,
/// rounded down for tier eligibility).
///
/// # Errors
/// [`BootError::Budget`] — below the 2-core host floor, an override the
/// resolved binding cannot honor (out-of-bounds, never a silent clamp),
/// or (forced/oversubscribed configs) the derivation's own refusal.
pub fn plan(
    quota_cpus: f64,
    profile: FleetProfile,
    overrides: &BudgetOverrides,
) -> Result<FleetPlan, BootError> {
    // Tier eligibility floors the fractional quota (budget.rs floors at
    // 1.0 on detection; the tier check is against HOST_FLOOR_CORES).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "quota floors are small positive values (core counts)"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "the quota is floored at 1.0 before the cast"
    )]
    let quota_floor = quota_cpus.max(1.0).floor() as u64;
    if quota_floor < HOST_FLOOR_CORES {
        return Err(BootError::Budget(BudgetError::BelowHostFloor {
            quota: quota_cpus,
        }));
    }
    match profile {
        FleetProfile::Auto => match FleetBudget::derive(quota_cpus, overrides) {
            Ok(_) => {
                validate_io_workers(Binding::Pinned, overrides)?;
                Ok(FleetPlan {
                    id: PLAN_ID,
                    binding: Binding::Pinned,
                    oversubscribed: false,
                    budget_cpus: quota_cpus,
                    tier_refusal: None,
                })
            }
            // The 2-5-core tier: the plan says Serial, carrying the pinned
            // refusal that placed the host there. The arm lands with
            // FF-T4; until then the boot re-raises the tier refusal.
            Err(refusal @ BudgetError::QuotaTooSmallForPinnedRoles { .. }) => {
                validate_io_workers(Binding::Serial, overrides)?;
                Ok(FleetPlan {
                    id: PLAN_ID,
                    binding: Binding::Serial,
                    oversubscribed: false,
                    budget_cpus: quota_cpus,
                    tier_refusal: Some(refusal),
                })
            }
            // An oversubscribed / under-minimum pinned config is a
            // configuration bug on ANY tier: refuse with its own typed
            // error, never fall through to a narrower plan.
            Err(other) => Err(BootError::Budget(other)),
        },
        FleetProfile::Pinned => {
            validate_io_workers(Binding::Pinned, overrides)?;
            // Forced pinned runs on any host with 2 or more cores; below
            // the pinned-role floor the latency contract is void (marked).
            // The typed refusal the operator overrode RIDES the plan — like
            // the auto serial tier carries its placing refusal — so the
            // runtime status names the pinned floor it fell from (FF-T5
            // addendum, 452GZC).
            let (oversubscribed, tier_refusal) = match FleetBudget::derive(quota_cpus, overrides) {
                Ok(_) => (false, None),
                Err(refusal) => (true, Some(refusal)),
            };
            Ok(FleetPlan {
                id: PLAN_ID,
                binding: Binding::Pinned,
                oversubscribed,
                budget_cpus: quota_cpus,
                tier_refusal,
            })
        }
        FleetProfile::Serial => {
            validate_io_workers(Binding::Serial, overrides)?;
            Ok(FleetPlan {
                id: PLAN_ID,
                binding: Binding::Serial,
                oversubscribed: false,
                budget_cpus: quota_cpus,
                tier_refusal: None,
            })
        }
    }
}

/// The per-binding `runtime.io_workers` bounds (FF-T2: an out-of-bounds
/// override raises a typed refusal with a hint, never a silent clamp).
/// The pinned binding keeps the ambient floor (A >= 1); the serial
/// binding owns exactly ONE ambient I/O lane (A == 1).
fn validate_io_workers(binding: Binding, overrides: &BudgetOverrides) -> Result<(), BootError> {
    let Some(requested) = overrides.ambient_io_workers else {
        return Ok(());
    };
    let legal = match binding {
        Binding::Pinned => requested >= 1,
        Binding::Serial => requested == 1,
    };
    if legal {
        return Ok(());
    }
    Err(BootError::Budget(BudgetError::IoWorkersOutOfBounds {
        requested,
        binding: binding.name(),
    }))
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::dispatcher::SlotLayout;
    use degenbot_config::FleetProfile;

    fn overrides() -> BudgetOverrides {
        BudgetOverrides::default()
    }

    /// THE FF-T2 algebra table (the AC, verbatim): one pure function of
    /// the budget over the ordered host tiers.
    #[test]
    fn the_tier_table_is_the_contract() {
        // plan(1.5) -> Err: below the 2-core host floor (one core for
        // I/O work, one core for solve work), typed.
        let err = plan(1.5, FleetProfile::Auto, &overrides()).expect_err("sub-2-core host");
        assert!(
            matches!(err, BootError::Budget(BudgetError::BelowHostFloor { quota }) if (quota - 1.5).abs() < f64::EPSILON),
            "below-2-cores refuses with the typed host-floor error, got {err:?}"
        );
        // plan(2.0) -> Serial: the pinned derivation refuses (floor 2 <
        // the 6-core pinned-role floor), the host keeps the 2-core minimum.
        let p = plan(2.0, FleetProfile::Auto, &overrides()).expect("2-core host plans");
        assert_eq!(p.binding, Binding::Serial);
        assert!(!p.oversubscribed);
        assert_eq!(p.id, PLAN_ID);
        // plan(5.99) -> Serial: tier eligibility FLOORS the fraction.
        let p = plan(5.99, FleetProfile::Auto, &overrides()).expect("5.99-core host plans");
        assert_eq!(p.binding, Binding::Serial);
        // plan(6.0) -> Pinned: the pinned derivation succeeds exactly
        // there (H1+A1+R1+M1+S2 = 6).
        let p = plan(6.0, FleetProfile::Auto, &overrides()).expect("6-core host plans");
        assert_eq!(p.binding, Binding::Pinned);
        assert!(!p.oversubscribed);
        // Forced pinned on a 2-core quota -> Pinned + oversubscribed
        // (latency contract void, correctness intact, LOUD).
        let p = plan(2.0, FleetProfile::Pinned, &overrides()).expect("forced pinned plans");
        assert_eq!(p.binding, Binding::Pinned);
        assert!(
            p.oversubscribed,
            "a forced pinned binding below the floor is marked"
        );
        // Forced serial on a 24-core quota -> Serial (forced bindings
        // run on any host with 2 or more cores).
        let p = plan(24.0, FleetProfile::Serial, &overrides()).expect("forced serial plans");
        assert_eq!(p.binding, Binding::Serial);
        assert!(!p.oversubscribed);
        assert!(p.tier_refusal.is_none());
    }

    /// The auto serial tier CARRIES the pinned refusal that placed the
    /// host there: the boot re-raises it while the arm is pending
    /// (FF-T4), keeping the CI-stable typed message.
    #[test]
    fn the_auto_serial_tier_carries_the_pinned_refusal() {
        let p = plan(4.0, FleetProfile::Auto, &overrides()).expect("4-core host plans");
        assert_eq!(p.binding, Binding::Serial);
        let refusal = p.tier_refusal.as_ref().expect("the tier refusal rides");
        assert!(
            matches!(
                refusal,
                BudgetError::QuotaTooSmallForPinnedRoles { quota, required }
                    if (*quota - 4.0).abs() < f64::EPSILON && *required == 6
            ),
            "the carried refusal is the pinned derivation's own, got {refusal:?}"
        );
        // The boot-facing refusal is the SAME budget error (the
        // pending-arm gate), and a forced serial profile gets the
        // named pending invariant instead.
        assert!(matches!(
            p.pending_serial_refusal(),
            BootError::Budget(BudgetError::QuotaTooSmallForPinnedRoles { .. })
        ));
        let forced = plan(24.0, FleetProfile::Serial, &overrides()).expect("forced serial plans");
        assert!(matches!(
            forced.pending_serial_refusal(),
            BootError::Invariant(_)
        ));
    }

    /// FF-T5 addendum (452GZC): a forced pinned binding below the
    /// pinned-role floor is MARKED and KEEPS the typed refusal it overrode —
    /// the runtime status names the pinned floor it fell from, exactly like
    /// the auto serial tier carries its placing refusal. At/above the floor
    /// nothing was refused.
    #[test]
    fn the_forced_pinned_oversubscription_carries_the_pinned_floor_refusal() {
        let p = plan(4.0, FleetProfile::Pinned, &overrides()).expect("forced pinned plans");
        assert_eq!(p.binding, Binding::Pinned);
        assert!(p.oversubscribed);
        let refusal = p
            .tier_refusal
            .as_ref()
            .expect("the overridden refusal rides");
        assert!(
            matches!(
                refusal,
                BudgetError::QuotaTooSmallForPinnedRoles { quota, required }
                    if (*quota - 4.0).abs() < f64::EPSILON && *required == 6
            ),
            "the carried refusal is the pinned derivation's own, got {refusal:?}"
        );
        for quota in [6.0_f64, 8.0, 24.0] {
            let p = plan(quota, FleetProfile::Pinned, &overrides()).expect("eligible host plans");
            assert!(!p.oversubscribed);
            assert!(p.tier_refusal.is_none());
        }
    }

    /// `runtime.io_workers` is validated against the plan bounds: an
    /// out-of-bounds override raises a typed refusal with a hint, never
    /// a silent clamp. The pinned binding keeps the ambient floor
    /// (A >= 1); the serial binding owns exactly ONE ambient I/O lane.
    #[test]
    fn io_workers_overrides_are_validated_against_plan_bounds() {
        // Pinned + A=0: below the ambient floor.
        let ov = BudgetOverrides {
            ambient_io_workers: Some(0),
            ..overrides()
        };
        let err = plan(8.0, FleetProfile::Pinned, &ov).expect_err("A=0 refuses");
        assert!(
            matches!(
                err,
                BootError::Budget(BudgetError::IoWorkersOutOfBounds {
                    requested: 0,
                    binding: "pinned"
                })
            ),
            "the out-of-bounds override names the request and the binding, got {err:?}"
        );
        // Serial + A=2: off the exactly-one ambient I/O lane (the auto
        // serial tier validates the same bounds).
        let ov = BudgetOverrides {
            ambient_io_workers: Some(2),
            ..overrides()
        };
        let err = plan(4.0, FleetProfile::Auto, &ov).expect_err("serial A=2 refuses");
        assert!(
            matches!(
                err,
                BootError::Budget(BudgetError::IoWorkersOutOfBounds {
                    requested: 2,
                    binding: "serial"
                })
            ),
            "the serial tier validates its one-lane bound, got {err:?}"
        );
        // A legal override rides untouched (never a clamp).
        let ov = BudgetOverrides {
            ambient_io_workers: Some(2),
            ..overrides()
        };
        let p = plan(8.0, FleetProfile::Auto, &ov).expect("8-core A=2 plans");
        assert_eq!(p.binding, Binding::Pinned);
        assert_eq!(
            p.projected_budget(&ov)
                .expect("projection derives")
                .ambient_cpus,
            2
        );
    }

    /// The plan-tier invariants (the `SlotLayout` dead-station family
    /// extended into tiers): a serial-plan projection must yield a LEGAL,
    /// non-empty layout — 1 solver seat, 1 resolve, >= 1 poolupd, sim per
    /// budget; the forced-pinned marked projection must stay legal too.
    #[test]
    fn plan_projections_yield_legal_non_empty_layouts() {
        for quota in [2.0_f64, 2.5, 4.0, 5.99, 24.0] {
            let p = plan(quota, FleetProfile::Serial, &overrides()).expect("serial plans");
            let b = p
                .projected_budget(&overrides())
                .expect("serial projection is total");
            assert_eq!(b.solver_pin_count, 1, "serial-0: exactly one solve seat");
            assert_eq!(b.resolve_cpus, 1);
            assert!(b.pool_state_updater_slots >= 1);
            assert!(b.sim_slot_cap >= 1, "sim per budget");
            // The layout is legal (every hosted range non-empty, the merge
            // sidecar last); the seat counts were asserted on the budget
            // fields above — the ONE geometry derivation accepted them.
            SlotLayout::of(&b).expect("the serial projection hosts a legal layout");
        }
        // The forced-pinned marked projection: legal stations, the pin
        // count floors at 1, the mark carries the deficit story.
        for quota in [2.0_f64, 4.0, 5.99] {
            let p = plan(quota, FleetProfile::Pinned, &overrides()).expect("forced pinned plans");
            assert!(p.oversubscribed);
            let b = p
                .projected_budget(&overrides())
                .expect("the marked projection is total");
            assert!(
                b.solver_pin_count >= 1,
                "station legality: the pin count floors at 1"
            );
            assert!(
                (b.fractional_remainder - 0.0).abs() < 1e-9,
                "oversubscribed: no spendable remainder"
            );
            SlotLayout::of(&b).expect("the marked projection hosts a legal layout");
        }
    }

    /// The pinned eligible path is the ONE derivation, byte-stable: the
    /// projection IS `FleetBudget::derive` (no second arithmetic).
    #[test]
    fn the_pinned_eligible_projection_is_the_one_derivation() {
        for quota in [6.0_f64, 8.0, 24.0, 6.5] {
            let p = plan(quota, FleetProfile::Auto, &overrides()).expect("eligible host plans");
            assert_eq!(p.binding, Binding::Pinned);
            assert_eq!(
                p.projected_budget(&overrides())
                    .expect("projection derives"),
                FleetBudget::derive(quota, &overrides()).expect("the derivation")
            );
        }
    }
}
