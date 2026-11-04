//! The pre-sim gas floor as ONE admission decision, enforced at the two seams a
//! solved candidate meets.
//!
//! `degenbot_pools::solve::path_gas_floor_wei` prices a solved candidate's own
//! measured execution gas — its walk census and family rows, carried on the
//! result — at the block's base fee. [`admission_floor`] raises the operator's
//! static `min_profit_wei` by that figure, and [`admit`] takes the decision
//! once per candidate per seam, reporting the suppressions the GAS term caused.
//! A candidate under the static minimum alone is the pre-existing profit window
//! doing its job, not a gas-floor event, so it is not counted here.
//!
//! # Two seams, one decision
//!
//! - [`AdmissionSeam::InlineSim`] — the lane walk's inline-sim scheduling seam
//!   (`PipelinedSims::schedule_one`). A candidate under the floor earns no
//!   simulation: this is the seam that PREVENTS the work, and it is where the
//!   sub-gas flood is born (a phantom candidate that is simulated, fails, and
//!   is suppressed by the delivery window afterwards has already cost the sim).
//!
//! - [`AdmissionSeam::DeliveryWindow`] — the delivery policy's profit window. A
//!   candidate under the floor does not enter the batch. It stays as the second
//!   gate: the base fee can move between the solve and the publish, and a
//!   candidate that never rode the walk's inline admission (a registration-time
//!   eager solve, a cold-start sweep, an engine with the inline stance off)
//!   meets no other gate.
//!
//! A candidate floored at the inline seam still flows to delivery — its held
//! outcome flushes with no payload — where the window floors it again. Both
//! seams read the same static minimum, the same result, and (in steady state)
//! the same block metadata, so with the inline gate armed the window's decision
//! is the inline decision re-derived at publish: ONE suppression, not two.
//! [`AdmissionSeam::of_cycle`] therefore names one COUNTING seam per cycle — the
//! inline gate when the walk's stance is on and the hook is installed, the
//! window otherwise — so a floored candidate increments the counter exactly
//! once, at the seam that made the decision.
//!
//! # The hooked leg: measured gas replaces the hookless row
//!
//! The cost model's V4 row is hookless, so a hooked leg's own work is invisible
//! to it and the candidate clears the floor only to fail net-of-hook-gas at
//! verification. The finalist re-quote ([`super::hooked_leg_requote`]) prices
//! that leg black-box at the solve's anchor and its quote frame carries the
//! gas. [`commit_measured_leg_gas`] writes that figure into the result's hop
//! profile ([`HopGasProfile::Measured`]) for every leg the plan re-quoted
//! exact; a refused leg keeps the row. Both seams read the same result, so one
//! commit at the clamp reaches the inline gate AND the delivery window.
//!
//! # Units
//!
//! The floor is ETH wei (gas times the block's base fee) and `profit` is in the
//! path's terminal token, so the comparison is exact only for an 18-decimal
//! ether-equivalent terminal (the settlement engine's WETH-terminated paths).
//! The model has no price oracle and invents none.

use super::hooked_leg_requote::HookedLegPlan;
use ::degenbot_pools::solve::{HopGasProfile, SolvePathResult};
use alloy::primitives::U256;
use degenbot_core::op_debug;

/// The seam a candidate's admission floor is enforced at — the vocabulary the
/// suppression counter and its debug line are labelled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionSeam {
    /// The lane walk's inline-sim scheduling seam: no simulation is scheduled.
    InlineSim,
    /// The delivery policy's profit window: no batch entry is published.
    DeliveryWindow,
}

impl AdmissionSeam {
    /// The seam that owns the floor's COUNT for a cycle's candidates — the
    /// inline gate when the walk's stance is on AND the hook is installed, the
    /// delivery window otherwise (the only gate those candidates meet). See the
    /// module docs: the window's re-floor of an already-floored candidate is
    /// the same decision and must not be counted again.
    pub(crate) const fn of_cycle(inline_sim_enabled: bool, hook_installed: bool) -> Self {
        if inline_sim_enabled && hook_installed {
            Self::InlineSim
        } else {
            Self::DeliveryWindow
        }
    }

    /// The seam a suppression the DELIVERY WINDOW enforces is counted at, given
    /// the cycle's owning seam (`self`): `None` when the walk's inline gate owns
    /// the count — the window's floor is the upstream decision re-derived, not a
    /// second one — and the window itself otherwise.
    pub(crate) const fn window_count(self) -> Option<Self> {
        match self {
            Self::InlineSim => None,
            Self::DeliveryWindow => Some(Self::DeliveryWindow),
        }
    }

    /// The closed label set of the suppression counter and debug line.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::InlineSim => "inline",
            Self::DeliveryWindow => "delivery",
        }
    }
}

/// The floor one result must clear at `base_fee_per_gas`: the operator's static
/// minimum raised by the path's own gas cost.
///
/// The gas term is DATA — per path (its family rows and the walk's census, both
/// carried on the result) and per block (that block's base fee) — never a
/// constant, and it can only raise the bar. A result the model cannot price
/// (no base fee on the block, or no hop projection: a family with no measured
/// coefficient row, or a hand-built result) falls back to the static minimum
/// rather than an invented cost.
pub(crate) fn admission_floor(
    result: &SolvePathResult,
    min_profit: U256,
    base_fee_per_gas: Option<u64>,
) -> U256 {
    let gas_floor = base_fee_per_gas.and_then(|base_fee| {
        ::degenbot_pools::solve::path_gas_floor_wei(result, u128::from(base_fee))
    });
    gas_floor.map_or(min_profit, |gas_floor| min_profit.max(gas_floor))
}

/// Commit a finalist re-quote's MEASURED leg gas onto the result's floor
/// profiles: every hop the plan re-quoted exact at the anchor becomes
/// [`HopGasProfile::Measured`], which the cost model charges verbatim in place
/// of the hookless V4 row (see `degenbot_pools::solve::gas_floor`'s module docs
/// for why it replaces rather than adds — the row was fitted on that same quote
/// frame).
///
/// A hop the plan left approximate (no tier installed, a refusal, a hook the
/// frame cannot execute) keeps whatever profile the solve recorded, so the
/// candidate keeps its honest under-prediction. Hops the plan does not name —
/// every non-hooked leg — are untouched.
///
/// The commit belongs at the CLAMP (the one body both seams share an upstream
/// of): it is the re-quote's own commit point, and it writes the same result the
/// inline gate and the delivery window price. Out-of-range hops (a plan built
/// over a longer pool list than the result carries projections for) are
/// ignored rather than panicking: the result simply keeps its rows.
pub(crate) fn commit_measured_leg_gas(result: &mut SolvePathResult, plan: &HookedLegPlan) {
    for (hop, profile) in result.gas_hops.iter_mut().enumerate() {
        if let Some(gas) = plan.measured_gas(hop) {
            *profile = HopGasProfile::Measured { gas };
        }
    }
}

/// Admit-or-floor one solved result, reporting a candidate the GAS floor
/// excluded (a counter labelled with the seam plus a debug line naming the path,
/// its margin, the floor, and the block's base fee — never a silent drop).
///
/// `counted_at` says whether THIS enforcement is the decision the counter owns:
/// `Some(seam)` counts and labels the suppression, `None` re-derives a decision
/// already counted upstream ([`AdmissionSeam::window_count`]) and stays silent —
/// the gate still excludes, but one candidate is one decision.
///
/// The decision is strict: a margin exactly ON the floor is excluded.
pub(crate) fn admit(
    counted_at: Option<AdmissionSeam>,
    path_id: u64,
    result: &SolvePathResult,
    min_profit: U256,
    base_fee_per_gas: Option<u64>,
) -> bool {
    let floor = admission_floor(result, min_profit, base_fee_per_gas);
    if result.profit > floor {
        return true;
    }
    if floor > min_profit {
        if let Some(seam) = counted_at {
            crate::instruments::record(crate::instruments::CensusEvent::AdmissionFloored {
                seam: seam.label(),
            });
        }
        op_debug!(
            domain = solver,
            path_id,
            seam = counted_at.map_or("(counted upstream)", AdmissionSeam::label),
            profit = %result.profit,
            floor = %floor,
            min_profit = %min_profit,
            base_fee_per_gas = base_fee_per_gas.unwrap_or(0),
            "admission: candidate floored — its margin does not cover the path's measured \
             execution gas at this block's base fee"
        );
    }
    false
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;
    use ::degenbot_pools::solve::{HopGasProfile, MEASURED_GAS_FLOOR};

    /// Two V2 legs: the cheapest priceable path shape (a flat leg each, no tick
    /// map, no census).
    const BASE_FEE: u64 = 12_000_000_000;

    fn priced_result(profit: u64) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit: U256::from(profit),
            gas_hops: vec![HopGasProfile::V2, HopGasProfile::V2],
            ..Default::default()
        }
    }

    fn two_v2_hop_floor(base_fee: u64) -> U256 {
        MEASURED_GAS_FLOOR
            .floor_wei(
                &[HopGasProfile::V2, HopGasProfile::V2],
                u128::from(base_fee),
            )
            .unwrap()
    }

    /// The decision is the static minimum raised by the path's own gas cost, and
    /// the gas term is the block's base fee times the path's measured gas.
    #[test]
    fn the_floor_is_the_static_minimum_raised_by_the_path_gas() {
        let result = priced_result(0);
        let gas_floor = two_v2_hop_floor(BASE_FEE);
        assert_eq!(
            gas_floor,
            U256::from(176_364u64) * U256::from(BASE_FEE),
            "two V2 legs + per-hop transfers + the settlement frame"
        );
        assert_eq!(
            admission_floor(&result, U256::ZERO, Some(BASE_FEE)),
            gas_floor
        );
        assert_eq!(
            admission_floor(&result, gas_floor * U256::from(2u64), Some(BASE_FEE)),
            gas_floor * U256::from(2u64),
            "a static minimum above the gas floor stays the floor"
        );
        assert_eq!(
            admission_floor(&result, U256::ZERO, None),
            U256::ZERO,
            "no base fee on the block → the static minimum alone"
        );
    }

    /// An unpriceable result keeps the pre-existing static window: no floor is
    /// better than an invented one.
    #[test]
    fn an_unpriceable_result_keeps_the_static_minimum() {
        let result = SolvePathResult {
            profit: U256::from(50u64),
            ..Default::default()
        };
        assert!(result.gas_hops.is_empty());
        assert_eq!(
            admission_floor(&result, U256::from(100u64), Some(BASE_FEE)),
            U256::from(100u64)
        );
    }

    /// The decision is strict at the boundary: ON the floor is out, one wei over
    /// is in.
    #[test]
    fn the_boundary_is_strict() {
        let floor = two_v2_hop_floor(BASE_FEE);
        let on_the_floor = u64::try_from(floor).unwrap();
        assert!(!admit(
            Some(AdmissionSeam::InlineSim),
            1,
            &priced_result(on_the_floor),
            U256::ZERO,
            Some(BASE_FEE)
        ));
        assert!(admit(
            Some(AdmissionSeam::InlineSim),
            1,
            &priced_result(on_the_floor + 1),
            U256::ZERO,
            Some(BASE_FEE)
        ));
        // Below the STATIC minimum alone: excluded, and not a gas-floor event.
        assert!(!admit(
            Some(AdmissionSeam::DeliveryWindow),
            1,
            &priced_result(50),
            U256::from(100u64),
            Some(BASE_FEE)
        ));
    }

    /// The counting seam is one per cycle: the inline gate owns the count when
    /// it is armed, the delivery window when it is not. Both seams read the same
    /// minimum, result, and base fee, so the window's re-floor of a
    /// floored-at-inline candidate is the same decision — counted once.
    #[test]
    fn the_counting_seam_is_one_per_cycle() {
        assert_eq!(
            AdmissionSeam::of_cycle(true, true),
            AdmissionSeam::InlineSim,
            "stance on + hook installed → the walk's gate decides"
        );
        assert_eq!(
            AdmissionSeam::of_cycle(true, false),
            AdmissionSeam::DeliveryWindow,
            "stance on but no hook → no inline gate runs, the window decides"
        );
        assert_eq!(
            AdmissionSeam::of_cycle(false, true),
            AdmissionSeam::DeliveryWindow,
            "stance off → the window decides"
        );
        assert_eq!(AdmissionSeam::InlineSim.label(), "inline");
        assert_eq!(AdmissionSeam::DeliveryWindow.label(), "delivery");
        // The count-once rule: an armed cycle's window suppression is the
        // inline decision re-derived, so the window records nothing; an unarmed
        // cycle's window is the only gate and records itself.
        assert_eq!(AdmissionSeam::of_cycle(true, true).window_count(), None);
        assert_eq!(
            AdmissionSeam::of_cycle(false, true).window_count(),
            Some(AdmissionSeam::DeliveryWindow)
        );
        assert_eq!(
            AdmissionSeam::of_cycle(true, false).window_count(),
            Some(AdmissionSeam::DeliveryWindow)
        );
    }

    // ── the hooked leg: the re-quote's MEASURED gas in the floor ─────────

    use crate::arb_engine::hooked_leg_requote::{
        HookedLeg, HookedLegAnchor, HookedLegAnswer, HookedLegPlan, HookedLegQuote,
        HookedLegReQuote,
    };

    /// The frame gas a hooked leg's black-box quote reports: far above the
    /// hookless V4 row (72,232) because the hook's own work is inside it.
    const MEASURED_HOOK_GAS: u64 = 264_000;

    /// The hooked hop's index on these fixtures (a V2 leg in front of it, so a
    /// wrong-hop commit is visible).
    const HOOKED_HOP: usize = 1;

    /// A tier that answers every leg with the same fixed frame gas — the
    /// re-quote's shape, without an EVM.
    struct MeasuredTier;

    impl HookedLegReQuote for MeasuredTier {
        fn requote_leg(&self, _anchor: HookedLegAnchor, leg: &HookedLeg) -> HookedLegAnswer {
            HookedLegAnswer::Exact(HookedLegQuote {
                amount_out: leg.amount_in,
                gas: MEASURED_HOOK_GAS,
                hook_fee: None,
            })
        }
    }

    /// A tier that refuses (no scratch EVM, a foreign manager, a halted frame).
    struct RefusingTier;

    impl HookedLegReQuote for RefusingTier {
        fn requote_leg(&self, _anchor: HookedLegAnchor, _leg: &HookedLeg) -> HookedLegAnswer {
            HookedLegAnswer::Unavailable
        }
    }

    /// One hooked V4 leg's facts, hop-indexed to the fixtures below.
    fn hooked_fact() -> (usize, HookedLeg) {
        (
            HOOKED_HOP,
            HookedLeg {
                pool_manager: alloy::primitives::Address::ZERO,
                pool_key: degenbot_pools::V4PoolKey {
                    currency0: alloy::primitives::Address::ZERO,
                    currency1: alloy::primitives::Address::ZERO,
                    fee: 3_000,
                    tick_spacing: 60,
                    hooks: alloy::primitives::Address::ZERO,
                },
                zero_for_one: true,
                amount_in: U256::from(1_000u64),
                pool_id: 3,
            },
        )
    }

    fn hooked_plan(tier: Option<&dyn HookedLegReQuote>) -> HookedLegPlan {
        HookedLegPlan::requote(
            vec![hooked_fact()],
            HookedLegAnchor {
                block: 1,
                timestamp: 2,
                base_fee: u128::from(BASE_FEE),
            },
            tier,
        )
    }

    /// A V2 leg then a HOOKLESS V4 row — the profile a hooked leg carries
    /// before anything measured it.
    fn hooked_hops() -> Vec<HopGasProfile> {
        vec![HopGasProfile::V2, HopGasProfile::V4 { census: None }]
    }

    fn hooked_result(profit: U256) -> SolvePathResult {
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit,
            gas_hops: hooked_hops(),
            ..Default::default()
        }
    }

    /// The hookless leg's price, so the delta the hook adds is explicit.
    fn hookless_v4_leg() -> U256 {
        U256::from(
            MEASURED_GAS_FLOOR
                .leg_gas(HopGasProfile::V4 { census: None })
                .unwrap(),
        )
    }

    /// The re-quote's measured gas commits onto the result — the carrier both
    /// seams read — at the leg's own hop and nowhere else.
    #[test]
    fn a_re_quoted_hooked_leg_commits_the_measured_gas() {
        let tier = MeasuredTier;
        let plan = hooked_plan(Some(&tier));
        let mut result = hooked_result(U256::ZERO);
        commit_measured_leg_gas(&mut result, &plan);
        assert_eq!(
            result.gas_hops[HOOKED_HOP],
            HopGasProfile::Measured {
                gas: MEASURED_HOOK_GAS
            },
            "the measured frame gas replaces the hookless row"
        );
        assert_eq!(
            result.gas_hops[0],
            HopGasProfile::V2,
            "a leg the plan does not name keeps its profile"
        );
        assert_eq!(
            admission_floor(&result, U256::ZERO, Some(BASE_FEE)),
            MEASURED_GAS_FLOOR
                .floor_wei(
                    &[
                        HopGasProfile::V2,
                        HopGasProfile::Measured {
                            gas: MEASURED_HOOK_GAS
                        },
                    ],
                    u128::from(BASE_FEE),
                )
                .unwrap(),
            "the floor prices the measured figure, not the row"
        );
    }

    /// The regression this floor exists for: a hooked candidate whose margin
    /// clears the HOOKLESS floor is floored once the hook's own gas is charged.
    #[test]
    fn the_measured_gas_floors_a_candidate_the_hookless_row_admitted() {
        let before = hooked_result(U256::ZERO);
        let hookless_floor = admission_floor(&before, U256::ZERO, Some(BASE_FEE));
        // One wei over the hookless floor: admitted, as it always was.
        let margin = hookless_floor + U256::ONE;
        let mut result = hooked_result(margin);
        assert!(
            admit(
                Some(AdmissionSeam::InlineSim),
                11,
                &result,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "the hookless row prices this candidate as profitable"
        );

        let tier = MeasuredTier;
        let plan = hooked_plan(Some(&tier));
        commit_measured_leg_gas(&mut result, &plan);
        assert!(
            !admit(
                Some(AdmissionSeam::InlineSim),
                11,
                &result,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "the hook's own gas takes the candidate under the bar"
        );
        assert_eq!(
            admission_floor(&result, U256::ZERO, Some(BASE_FEE)) - hookless_floor,
            (U256::from(MEASURED_HOOK_GAS) - hookless_v4_leg()) * U256::from(BASE_FEE),
            "the floor rose by exactly the hook's measured execution cost"
        );
        // The delivery window derives the same figure from the same result.
        assert!(
            !admit(
                AdmissionSeam::of_cycle(false, true).window_count(),
                11,
                &result,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "the window floors it too — one candidate, one decision"
        );
    }

    /// A REFUSED re-quote has no measured gas, so the hookless row stands and
    /// the candidate keeps its honest under-prediction — it still ships.
    #[test]
    fn a_refused_requote_keeps_the_hookless_row() {
        let tier = RefusingTier;
        let plan = hooked_plan(Some(&tier));
        let before = hooked_result(U256::ZERO);
        let hookless_floor = admission_floor(&before, U256::ZERO, Some(BASE_FEE));
        let mut result = hooked_result(hookless_floor + U256::ONE);
        commit_measured_leg_gas(&mut result, &plan);
        assert_eq!(
            result.gas_hops,
            hooked_hops(),
            "a refusal writes nothing: the rows stay as the solve recorded them"
        );
        assert_eq!(
            admission_floor(&result, U256::ZERO, Some(BASE_FEE)),
            hookless_floor
        );
        assert!(
            admit(
                Some(AdmissionSeam::InlineSim),
                12,
                &result,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "the approximation stands, so the candidate is not suppressed"
        );
    }

    /// A hooked candidate whose margin covers its TRUE (measured) cost still
    /// ships: the measured gas raises the bar, it never vetoes hook work.
    #[test]
    fn a_hooked_candidate_covering_its_measured_cost_still_ships() {
        let tier = MeasuredTier;
        let plan = hooked_plan(Some(&tier));
        let mut probe = hooked_result(U256::ZERO);
        commit_measured_leg_gas(&mut probe, &plan);
        let measured_floor = admission_floor(&probe, U256::ZERO, Some(BASE_FEE));
        assert!(
            measured_floor
                > admission_floor(&hooked_result(U256::ZERO), U256::ZERO, Some(BASE_FEE)),
            "the fixture only tests anything if the measured gas raises the floor"
        );
        let mut result = hooked_result(measured_floor + U256::ONE);
        commit_measured_leg_gas(&mut result, &plan);
        assert!(
            admit(
                Some(AdmissionSeam::InlineSim),
                13,
                &result,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "a hooked candidate that covers its measured cost ships"
        );
        let mut on_the_floor = hooked_result(measured_floor);
        commit_measured_leg_gas(&mut on_the_floor, &plan);
        assert!(
            !admit(
                Some(AdmissionSeam::InlineSim),
                13,
                &on_the_floor,
                U256::ZERO,
                Some(BASE_FEE)
            ),
            "ON the composed floor is still out (the strict boundary)"
        );
    }

    /// The commit is bounded by the result it writes: a plan naming a hop the
    /// result carries no projection for writes nothing and never panics.
    #[test]
    fn the_commit_ignores_hops_the_result_does_not_carry() {
        let tier = MeasuredTier;
        let plan = hooked_plan(Some(&tier));
        let mut result = SolvePathResult {
            profit: U256::from(50u64),
            ..Default::default()
        };
        commit_measured_leg_gas(&mut result, &plan);
        assert!(result.gas_hops.is_empty());
        assert_eq!(
            admission_floor(&result, U256::from(100u64), Some(BASE_FEE)),
            U256::from(100u64),
            "an unpriceable result keeps the static minimum"
        );
    }

    /// The same result at the same base fee gets the SAME decision from both
    /// seams — the property that lets the window's echo go uncounted.
    #[test]
    fn both_seams_derive_the_same_floor() {
        let result =
            priced_result(u64::try_from(two_v2_hop_floor(BASE_FEE) / U256::from(2u64)).unwrap());
        let owner = AdmissionSeam::of_cycle(true, true);
        let inline = admit(
            Some(AdmissionSeam::InlineSim),
            7,
            &result,
            U256::ZERO,
            Some(BASE_FEE),
        );
        let delivery = admit(owner.window_count(), 7, &result, U256::ZERO, Some(BASE_FEE));
        assert_eq!(inline, delivery, "one decision, enforced at two seams");
        assert!(!inline);
    }
}
