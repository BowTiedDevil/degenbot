//! The Balancer-weighted family module (ADR-071 D1).
//!
//! GLOSSARY.md "Family module": the solver crate's single module owning
//! everything the Balancer V2 weighted hop family contributes to a solve and
//! its bound.
//!
//! What lives here:
//! - the family's resolved state slice ([`BalancerWeightedHopState`]);
//! - the family's gate projection ([`weighted_to_math`]) - weighted to
//!   `HopMath::Weighted`;
//! - the family's path-level solve entry ([`solve_balancer_weighted_path_int`])
//!   and its dispatch-row adapter ([`solve_balancer_weighted_entry`]), built on
//!   the shared golden-section template that stays family-blind in `mixed::solve`;
//! - the V2-equivalent projection ([`balancer_weighted_hop_v2_equiv`]) that
//!   template's Mobius precheck consumes;
//! - the simulate triple ([`simulate_balancer_weighted_path`] / `_outputs` /
//!   [`simulate_balancer_weighted_hop`]) over the `degenbot-balancer-math`
//!   weighted leaf;
//! - the family's envelope lines ([`weighted_hop_lines`]) - the concave entry
//!   tangent plus the asymptotic reserve cap.

use alloy::primitives::{aliases::I512, U256, U512};

use degenbot_math::balancer::constants::PowVersion;

use crate::mixed::solve::solve_golden_section_path_int;
use crate::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
use crate::profit_envelope::{GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache};
use crate::runtime::SolveRuntimeConfig;

/// Resolved state for a Balancer V2 weighted pool hop, ready for the
/// weighted solve branch. Carries everything the
/// `degenbot-balancer-math::weighted_math::calc_out_given_in` leaf needs.
///
/// The fee is stored as a fraction of `ONE = 1e18` (the Balancer fixed-point
/// convention): `amount_in_after_fee = amount_in - amount_in * swap_fee / ONE`.
/// The `PowVersion` selects `FixedPoint::pow` fast paths (V1 general / V2
/// fast paths for y ∈ {1, 2, 4}).
///
/// For the V2-equivalent Möbius precheck, `gamma_numer` / `fee_denom` give
/// the retained fraction `gamma_numer / fee_denom` = `(ONE - swap_fee) / ONE`
/// scaled to a `u64` pair (the `IntHopState` fee convention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerWeightedHopState {
    /// Balance of the input token (upscaled to 18-decimal fixed-point).
    pub balance_in: U256,
    /// Balance of the output token (upscaled to 18-decimal fixed-point).
    pub balance_out: U256,
    /// Normalized weight of the input token (18-decimal, e.g. 5e17 for 50%).
    pub weight_in: U256,
    /// Normalized weight of the output token (18-decimal).
    pub weight_out: U256,
    /// Swap fee as a fraction of `ONE = 1e18` (e.g. 3e15 for 0.3%).
    pub swap_fee: U256,
    /// `PowVersion` discriminator — selects `FixedPoint::pow` fast paths.
    pub pow_version: PowVersion,
    /// Scaling factor for the input token (`10^(18 - decimals_in)`).
    /// The input amount (in native decimals) is multiplied by this before
    /// passing to the math leaf, and the math result is divided by the
    /// output's scaling factor to downscale back to native decimals.
    pub scaling_factor_in: U256,
    /// Scaling factor for the output token (`10^(18 - decimals_out)`).
    pub scaling_factor_out: U256,
}

/// The Balancer-weighted family's gate projection: the concave weighted-pool
/// curve → entry-rate tangent dominates globally (sound) + the reserve
/// asymptote caps it.
pub(crate) fn weighted_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::BalancerWeighted { state } => Some(HopMath::Weighted {
            balance_in: state.balance_in,
            balance_out: state.balance_out,
            weight_in: state.weight_in,
            weight_out: state.weight_out,
            scaling_in: state.scaling_factor_in,
            scaling_out: state.scaling_factor_out,
        }),
        _ => None,
    }
}

/// The Balancer-weighted family's dispatch-row adapter for its
/// single-argument path entry (`SolveFn`): wraps
/// [`solve_balancer_weighted_path_int`] and reports no walk telemetry (the
/// golden-section arm does not walk).
pub(crate) fn solve_balancer_weighted_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    (
        solve_balancer_weighted_path_int(resolved),
        crate::cl::WalkStats::default(),
    )
}

/// Solve an all-V2-or-Balancer-weighted (no CL hops, ≥1 weighted) path with
/// the shared golden-section template (`solve_golden_section_path_int`).
///
/// Per-hop simulation calls
/// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The
/// V2-equivalent precheck treats each weighted hop as a constant-product
/// hop — exact for 50/50 pools, approximate for other weights (good enough
/// for bracket narrowing).
fn solve_balancer_weighted_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;
    if hops.len() < 2 {
        return None;
    }

    // V2-equivalent IntHopState per hop: weighted hops use
    // balance_in/balance_out + the retained fee fraction; V2 hops pass
    // through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(balancer_weighted_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(
        &v2_equiv,
        &|x| simulate_balancer_weighted_path(x, hops),
        &|x| simulate_balancer_weighted_path_outputs(x, hops),
    )
}

/// Build the V2-equivalent [`IntHopState`] for a hop in a Balancer
/// weighted solve path. Weighted hops use the raw balances as reserves
/// and convert the fee (`swap_fee` as fraction of 1e18 → retained fraction
/// `gamma_numer/fee_denom`). V2 hops pass through unchanged. Returns `None`
/// for any non-V2/non-weighted hop.
fn balancer_weighted_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::BalancerWeighted { state } => {
            // swap_fee is the fee fraction as parts of 1e18.
            // gamma = 1e18 - swap_fee; IntHopState wants gamma_numer/fee_denom.
            let one_e18 = U256::from(10u64).pow(U256::from(18u64));
            let gamma = one_e18.saturating_sub(state.swap_fee);
            let gn: u64 = gamma.try_into().ok()?;
            let fd: u64 = one_e18.try_into().ok()?;
            Some(IntHopState::new(
                state.balance_in,
                state.balance_out,
                gn,
                fd,
            ))
        }
        _ => None,
    }
}

/// Simulate a Balancer-weighted-or-V2 path, returning only the final output.
#[must_use]
pub(crate) fn simulate_balancer_weighted_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    simulate_balancer_weighted_path_outputs(x, hops)
        .last()
        .copied()
        .unwrap_or(U256::ZERO)
}

/// Simulate a Balancer-weighted-or-V2 path, returning per-hop outputs.
fn simulate_balancer_weighted_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::BalancerWeighted { state } => {
                simulate_balancer_weighted_hop(amount, state)
            }
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Evaluate a single Balancer weighted hop's output via
/// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The swap
/// fee is subtracted from the input first (Balancer convention:
/// `amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee)`),
/// then the invariant math runs on the after-fee amount. Returns
/// `U256::ZERO` on overflow / error (matching the defensive fallback in
/// `mixed::families::solidly::simulate_solidly_hop`).
fn simulate_balancer_weighted_hop(amount_in: U256, hop: &BalancerWeightedHopState) -> U256 {
    use degenbot_math::balancer::fixed_point::mul_up;
    use degenbot_math::balancer::weighted_math::calc_out_given_in;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    // Upscale input to 18-decimal fixed-point (plain integer multiply,
    // NOT mul_down — scaling factors are integer multipliers, not FXP).
    let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
    // Subtract swap fee: amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee).
    let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
        return U256::ZERO;
    };
    let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
    let Ok(amount_out_upscaled) = calc_out_given_in(
        hop.balance_in,
        hop.weight_in,
        hop.balance_out,
        hop.weight_out,
        amount_in_after_fee,
        hop.pow_version,
    ) else {
        return U256::ZERO;
    };
    // Downscale output from 18-decimal to native decimals (plain integer
    // division, NOT div_down — matches Python's `amount // scaling_factor`).
    if hop.scaling_factor_out.is_zero() {
        return U256::ZERO;
    }
    amount_out_upscaled / hop.scaling_factor_out
}

/// The Balancer-weighted family's line builder: the concave entry tangent plus
/// the asymptotic reserve cap.
pub(crate) fn weighted_hop_lines(
    hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    let HopMath::Weighted {
        balance_in,
        balance_out,
        weight_in,
        weight_out,
        scaling_in,
        scaling_out,
    } = hop
    else {
        return None;
    };
    weighted_lines(
        *balance_in,
        *balance_out,
        *weight_in,
        *weight_out,
        *scaling_in,
        *scaling_out,
    )
}

/// Build the line set for a Balancer V2 WEIGHTED hop: `[rise, flat]`.
///
/// The rise slope (native units) is
/// `B_out·sf_in·w_in/(B_in·w_out·sf_out)`, computed in `U512` to avoid
/// overflow and then narrowed to `I512`; the flat cap is the asymptotic
/// reserve `B_out/sf_out`.
fn weighted_lines(
    balance_in: U256,
    balance_out: U256,
    weight_in: U256,
    weight_out: U256,
    scaling_in: U256,
    scaling_out: U256,
) -> Option<(Vec<Line>, U256)> {
    if balance_in.is_zero()
        || balance_out.is_zero()
        || weight_out.is_zero()
        || scaling_out.is_zero()
    {
        return None;
    }
    let slope_num = U512::from(balance_out)
        .saturating_mul(U512::from(scaling_in))
        .saturating_mul(U512::from(weight_in));
    let slope_den = U512::from(balance_in)
        .saturating_mul(U512::from(weight_out))
        .saturating_mul(U512::from(scaling_out));
    if slope_den.is_zero() {
        return None;
    }
    let rise = Line {
        a: I512::ZERO,
        b: I512::try_from(slope_num).ok()?,
        c: I512::try_from(slope_den).ok()?,
    };
    // Cap (native) = B_out / sf_out (the asymptotic reserve).
    let cap = balance_out / scaling_out;
    let flat = Line {
        a: I512::try_from(U512::from(cap)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };
    Some((vec![rise, flat], cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    use crate::profit_envelope::path_output_bound_at;
    use degenbot_math::balancer::constants::ONE;

    proptest! {
        /// Balancer weighted: the concave tangent (entry marginal rate) + the
        /// reserve asymptote must dominate `calc_out_given_in` for every amount.
        #[test]
        fn m6776w_balancer_weighted_bound_dominates_real_leaf(
            b_in in 1u64..1_000_000_000u64,
            b_out in 1u64..1_000_000_000u64,
            w_in_pct in 10u32..90u32,
            x in 0u64..1_000_000_000u64,
        ) {
            let weight_in = U256::from(w_in_pct) * ONE / U256::from(100u32);
            let weight_out = ONE - weight_in;
            let state = crate::mixed::BalancerWeightedHopState {
                balance_in: U256::from(b_in),
                balance_out: U256::from(b_out),
                weight_in,
                weight_out,
                swap_fee: U256::from(3u64) * ONE / U256::from(1000u64),
                pow_version: degenbot_math::balancer::constants::PowVersion::V2,
                scaling_factor_in: U256::from(1u64),
                scaling_factor_out: U256::from(1u64),
            };
            let hop = HopMath::Weighted {
                balance_in: state.balance_in,
                balance_out: state.balance_out,
                weight_in: state.weight_in,
                weight_out: state.weight_out,
                scaling_in: state.scaling_factor_in,
                scaling_out: state.scaling_factor_out,
            };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let true_out = simulate_balancer_weighted_hop(U256::from(x), &state);
            // Skip cases the math leaf itself rejects (returns 0 with x>0):
            // those are not soundness-relevant.
            if x > 0 && true_out.is_zero() {
                return Ok(());
            }
            prop_assert!(
                bound >= true_out,
                "weighted bound {bound} < true {true_out} (bin={b_in},bout={b_out},w={w_in_pct},x={x})"
            );
        }
    }
}
