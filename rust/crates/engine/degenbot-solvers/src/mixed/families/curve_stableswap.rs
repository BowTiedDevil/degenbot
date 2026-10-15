//! The Curve stableswap family module (ADR-071 D1).
//!
//! GLOSSARY.md "Family module": the solver crate's single module owning
//! everything the Curve stableswap hop family contributes to a solve and its
//! bound.
//!
//! What lives here:
//! - the family's resolved state slice ([`CurveStableswapHopState`]);
//! - the family's gate projection ([`curve_to_math`]) - Curve to
//!   `HopMath::CurveStableswap`;
//! - the family's path-level solve entry ([`solve_curve_path_int`]) and its
//!   dispatch-row adapter ([`solve_curve_entry`]), built on the shared
//!   golden-section template that stays family-blind in `mixed::solve`;
//! - the V2-equivalent projection ([`curve_hop_v2_equiv`]) that template's
//!   Mobius precheck consumes;
//! - the simulate triple ([`simulate_curve_path`] / `_outputs` /
//!   [`simulate_curve_hop`]) over the `degenbot-math` Curve stableswap leaf;
//! - the family's envelope lines ([`curve_hop_lines`]): the NATIVE reserve cap
//!   ALONE. The entry-tangent shape was MEASURED non-concave against the real
//!   leaf (see [`curve_stableswap_lines`]), so an entry tangent would under-cut
//!   a real leaf and skip a profitable path.

use alloy::primitives::{aliases::I512, U256, U512};

use degenbot_math::curve::stableswap::{DVariant, YVariant};

use crate::mixed::solve::solve_golden_section_path_int;
use crate::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
use crate::profit_envelope::{GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache};
use crate::runtime::SolveRuntimeConfig;

/// Resolved state for a Curve stableswap pool hop, ready for the curve
/// solve branch.
///
/// Carries everything needed to simulate a swap: the rate-adjusted XP
/// array, the input/output token indices, amp (with `A_PRECISION` baked in),
/// fee, and the y/d variant discriminators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveStableswapHopState {
    /// `amp = a_coefficient * A_PRECISION` (Curve convention).
    pub amp: U256,
    /// `A_PRECISION` (100 for standard pools).
    pub a_precision: U256,
    /// Rate-adjusted balances `xp[i] = balances[i] * rate_multipliers[i] / PRECISION`.
    /// Pre-computed at resolve time.
    pub xp: Vec<U256>,
    /// Token index of the input.
    pub token_index_in: usize,
    /// Token index of the output.
    pub token_index_out: usize,
    /// Number of coins (== `xp.len()`).
    pub n_coins: U256,
    /// Swap fee in `FEE_DENOMINATOR = 1e10` units.
    pub fee: U256,
    /// `FEE_DENOMINATOR = 10^10`.
    pub fee_denom: U256,
    /// `PRECISION = 10^18`.
    pub precision: U256,
    /// Rate multiplier for the input token.
    pub rate_multiplier_in: U256,
    /// Rate multiplier for the output token.
    pub rate_multiplier_out: U256,
    /// y-iteration variant.
    pub y_variant: YVariant,
    /// d-iteration variant.
    pub d_variant: DVariant,
}

/// The Curve family's gate projection: the entry tangent was MEASURED
/// non-concave against the real leaf (see `curve_stableswap_lines`), so the
/// family keeps the sound native reserve cap — carried through its own hop
/// view.
pub(crate) fn curve_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::CurveStableswap { state } => {
            if state.rate_multiplier_out.is_zero() {
                return None;
            }
            // Only the fields the cap derivation and the family label read:
            // the rest of the resolved state stays on `CurveStableswapHopState`
            // for `simulate_curve_hop`, but the gate view does not carry it.
            Some(HopMath::CurveStableswap {
                amp: state.amp,
                xp: state.xp.as_slice(),
                token_index_in: state.token_index_in,
                token_index_out: state.token_index_out,
                precision: state.precision,
                rate_multiplier_out: state.rate_multiplier_out,
            })
        }
        _ => None,
    }
}

/// The Curve family's dispatch-row adapter for its single-argument path entry
/// (`SolveFn`): wraps [`solve_curve_path_int`] and reports no walk telemetry
/// (the golden-section arm does not walk).
pub(crate) fn solve_curve_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> crate::mixed::SolveArmOutcome {
    crate::mixed::SolveArmOutcome::walked(
        solve_curve_path_int(resolved),
        crate::concentrated_liquidity::WalkStats::default(),
    )
}

/// Curve stableswap solve — the shared golden-section template
/// (`solve_golden_section_path_int`) over the Curve stableswap math leaf
/// (`stableswap_get_y` + fee/rate conversion).
fn solve_curve_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;

    // V2-equivalent IntHopState per hop: Curve hops use xp[idx_in]/xp[idx_out]
    // + the retained fee fraction; V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(curve_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(&v2_equiv, &|x| simulate_curve_path(x, hops), &|x| {
        simulate_curve_path_outputs(x, hops)
    })
}

/// V2-equivalent [`IntHopState`] for a Curve stableswap hop.
fn curve_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::CurveStableswap { state } => {
            // Curve fee is fee/fee_denom; gamma = 1 - fee/fee_denom.
            // IntHopState wants gamma_numer/fee_denom.
            let gn: u64 = state.fee_denom.saturating_sub(state.fee).try_into().ok()?;
            let fd: u64 = state.fee_denom.try_into().ok()?;
            Some(IntHopState::new(
                state.xp[state.token_index_in],
                state.xp[state.token_index_out],
                gn,
                fd,
            ))
        }
        _ => None,
    }
}

/// Simulate the full path and return the final output.
fn simulate_curve_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    let outputs = simulate_curve_path_outputs(x, hops);
    outputs.last().copied().unwrap_or(U256::ZERO)
}

/// Simulate the full path and return each hop's output.
fn simulate_curve_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::CurveStableswap { state } => simulate_curve_hop(amount, state),
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Simulate a single Curve stableswap hop (rate-adjusted XP, `get_y`,
/// fee+rate conversion).
fn simulate_curve_hop(amount_in: U256, hop: &CurveStableswapHopState) -> U256 {
    use degenbot_math::curve::stableswap::stableswap_get_y;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    let i = hop.token_index_in;
    let j = hop.token_index_out;
    // Rate-adjust the input: x = xp[i] + dx * rate_in / PRECISION
    let x =
        hop.xp[i].saturating_add(amount_in.saturating_mul(hop.rate_multiplier_in) / hop.precision);
    let Ok(y) = stableswap_get_y(
        i,
        j,
        x,
        &hop.xp,
        hop.amp,
        hop.n_coins,
        hop.a_precision,
        hop.y_variant,
        hop.d_variant,
    ) else {
        return U256::ZERO;
    };
    // dy = xp[j] - y - 1
    let dy_raw = hop.xp[j].saturating_sub(y).saturating_sub(U256::from(1u64));
    // Fee on dy, then rate convert: (dy - fee) * PRECISION / rate_out
    let fee = dy_raw.saturating_mul(hop.fee) / hop.fee_denom;
    let dy_after_fee = dy_raw.saturating_sub(fee);
    // Rate-convert from XP-space to native token space
    dy_after_fee.saturating_mul(hop.precision) / hop.rate_multiplier_out
}

/// The Curve family's line builder: the native reserve cap alone — its entry
/// tangent was measured non-concave against the real leaf (see
/// `curve_stableswap_lines`).
pub(crate) fn curve_hop_lines(
    hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    let HopMath::CurveStableswap {
        xp,
        token_index_out,
        precision,
        rate_multiplier_out,
        ..
    } = hop
    else {
        return None;
    };
    curve_stableswap_lines(xp, *token_index_out, *precision, *rate_multiplier_out)
}

/// Build the line set for a Curve stableswap hop: `[flat]` — the NATIVE reserve
/// cap alone, by a VERIFIED non-concavity finding.
///
/// The entry-tangent shape was evaluated against the real
/// `degenbot_math::curve::stableswap` leaves (`stableswap_get_y` over all
/// `DVariant`/`YVariant` pairs) and REJECTED: the leaf's y-iteration floors
/// (`div`) and can settle a unit below the invariant's root, so
/// `dy_raw = xp_out − y − 1` reports more than the ideal curve. The measured
/// overshoot is 1–2 native units for the `Standard` y-iteration (extreme
/// amplification, nearly-empty co-coin), and grows to ~1e24 native units — the
/// pool's whole output reserve, at a 1-wei input — when a
/// `Variant0`/`Variant1` y-iteration (which omits the `A_PRECISION` divisor)
/// is paired with a `Standard` D-step. A tangent that under-cuts a real leaf is
/// an unsound skip, so the family keeps the sound cap. The counterexamples are
/// pinned in `tests/profit_envelope_tests.rs`.
fn curve_stableswap_lines(
    xp: &[U256],
    token_index_out: usize,
    precision: U256,
    rate_multiplier_out: U256,
) -> Option<(Vec<Line>, U256)> {
    if rate_multiplier_out.is_zero() {
        return None;
    }
    let xp_out = *xp.get(token_index_out)?;
    // Native output reserve = `xp[out]·PRECISION/rate_out`.
    let reserve_out = xp_out.saturating_mul(precision) / rate_multiplier_out;
    if reserve_out.is_zero() {
        return None;
    }
    let flat = Line {
        a: I512::try_from(U512::from(reserve_out)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };
    Some((vec![flat], reserve_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    use crate::profit_envelope::path_output_bound_at;

    proptest! {
        /// Curve stableswap: the reserve cap (`xp[out]·PRECISION/rate_out` = the
        /// native output-token reserve) must dominate the real Curve leaf output
        /// for every amount — guards the rate-multiplier unit conversion.
        #[test]
        fn m6776w_curve_reserve_cap_dominates_real_leaf(
            bal_in in 1_000_000u64..1_000_000_000_000u64,
            bal_out in 1_000_000u64..1_000_000_000_000u64,
            x in 0u64..1_000_000_000_000u64,
            token_in in 0u8..2u8,
        ) {
            use degenbot_math::curve::stableswap::{DVariant, YVariant};
            let precision = U256::from(1_000_000_000_000_000_000u64); // 1e18
            // rate multiplier for a 6-decimal token = 10^30 (10^(18+12)).
            let rate = U256::from(1_000_000_000u64)
                * U256::from(1_000_000_000u64)
                * U256::from(1_000_000_000_000u64); // 1e9 * 1e9 * 1e12 = 1e30
            let (i, j) = if token_in == 0 { (0usize, 1usize) } else { (1, 0) };
            let balances = [
                if i == 0 { bal_in } else { bal_out },
                if i == 1 { bal_in } else { bal_out },
            ];
            // xp[k] = balance[k] * rate / precision
            let xp: Vec<U256> = balances
                .iter()
                .map(|&b| U256::from(b).saturating_mul(rate) / precision)
                .collect();
            // Sanity: the caller-side cap formula xp[j]*precision/rate_out
            // equals the native reserve (exact: 1e30/1e18=1e12, no rounding).
            let caller_cap = xp[j].saturating_mul(precision) / rate;
            prop_assert!(
                caller_cap == U256::from(balances[j]),
                "curve cap unit mismatch: caller {caller_cap} != reserve {}",
                U256::from(balances[j])
            );
            let hop = HopMath::ReserveCap {
                reserve_out: U256::from(balances[j]),
            };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let state = crate::mixed::CurveStableswapHopState {
                amp: U256::from(1000u64),
                a_precision: U256::from(100u64),
                xp: xp.clone(),
                token_index_in: i,
                token_index_out: j,
                n_coins: U256::from(2u64),
                fee: U256::from(4_000_000u64),
                fee_denom: U256::from(10_000_000_000u64),
                precision,
                rate_multiplier_in: rate,
                rate_multiplier_out: rate,
                y_variant: YVariant::Standard,
                d_variant: DVariant::Standard,
            };
            let true_out = simulate_curve_hop(U256::from(x), &state);
            if x > 0 && true_out.is_zero() {
                return Ok(()); // leaf rejected the input; not soundness-relevant
            }
            prop_assert!(
                bound >= true_out,
                "curve cap {bound} < true {true_out} (bin={bal_in},bout={bal_out},x={x},t={token_in})"
            );
        }
    }
}
