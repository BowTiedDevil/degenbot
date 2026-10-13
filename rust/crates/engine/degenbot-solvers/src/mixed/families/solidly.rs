//! The Solidly family module (ADR-071 D1).
//!
//! GLOSSARY.md "Family module": the solver crate's single module owning
//! everything the Solidly/Aerodrome/Camelot hop family contributes to a solve
//! and its bound.
//!
//! What lives here:
//! - the family's resolved state slice ([`SolidlyHopState`]);
//! - the family's gate projection ([`solidly_to_math`]) - Solidly to `HopMath`;
//! - the family's path-level solve entry ([`solve_solidly_path_int`]) and its
//!   dispatch-row adapter ([`solve_solidly_entry`]), built on the shared
//!   golden-section template that stays family-blind in `mixed::solve`;
//! - the V2-equivalent projection ([`solidly_hop_v2_equiv`]) that template's
//!   Mobius precheck consumes;
//! - the simulate triple ([`simulate_solidly_path`] / `_outputs` /
//!   [`simulate_solidly_hop`]) over the `degenbot-solidly-math` leaf;
//! - the family's envelope lines ([`solidly_hop_lines`]).
//!
//! The family's INTERNAL split is exactly the two `HopMath` arms it maps to.
//! Solidly VOLATILE is the constant-product family - it takes the fee-agnostic
//! V2 rise lines through the shared `mobius_lines` builder. Solidly STABLE is
//! concave in the input, so its entry tangent is a sound rise beside the
//! reserve cap ([`solidly_stable_lines`] / [`solidly_stable_rise`]). Both arms
//! live in this one module and share the one dispatch row.

use alloy::primitives::{aliases::I512, U256, U512};

use degenbot_math::solidly::SOLIDLY_MIN_SCALED_RESERVE;
use degenbot_uniswap::dex_identity::DexVariant;

use crate::mixed::solve::solve_golden_section_path_int;
use crate::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
use crate::profit_envelope::{
    mobius_lines, GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache,
};
use crate::runtime::SolveRuntimeConfig;

/// Resolved state for a Solidly/Aerodrome/Camelot hop, ready for the
/// Solidly solve branch. Carries everything the `degenbot-solidly-math`
/// leaf needs in the leaf's own contract shape: **un-oriented**
/// `reserves_0`/`reserves_1` + a `token_in` direction flag (matching
/// [`degenbot_math::solidly::calc_exact_in_stable_solidly`] etc. 1:1).
///
/// `variant` + `stable` together dispatch the math:
/// - `(AerodromeV2Stable, true)` → `calc_exact_in_stable_solidly`
/// - `(CamelotV2Stable, true)` → `calc_exact_in_stable_camelot`
/// - `(_, false)` → `calc_exact_in_volatile`
///
/// Decimals are carried as the **scale** (`10^decimals`, e.g. `1_000_000`
/// for a 6-decimal token) — what `calc_k` divides by. The `fee` pair is the
/// **fee fraction** (`amount_in_after_fee = amount_in - amount_in *
/// fee_numer / fee_denom`); the resolve arm converts each family's stored
/// fee shape into this convention (Aerodrome stores it directly as the fee
/// fraction; Camelot stores the retained-fraction gamma and the arm inverts
/// it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolidlyHopState {
    /// Reserve of token0 (un-oriented).
    pub reserves_0: U256,
    /// Reserve of token1 (un-oriented).
    pub reserves_1: U256,
    /// Decimals scale of token0 (`10^decimals`, e.g. `1_000_000`).
    pub decimals_0: U256,
    /// Decimals scale of token1 (`10^decimals`, e.g. `1_000_000`).
    pub decimals_1: U256,
    /// Direction flag: `0` swaps token0→token1, `1` swaps token1→token0.
    /// Mirrors `token_in` in [`degenbot_math::solidly::calc_exact_in_stable_solidly`].
    pub token_in: u8,
    /// Fee numerator — the fee fraction is `fee_numer / fee_denom`.
    pub fee_numer: U256,
    /// Fee denominator.
    pub fee_denom: U256,
    /// `true` for the stable (x³y + xy³) invariant, `false` for volatile
    /// (constant-product).
    pub stable: bool,
    /// DEX+variant discriminator — selects the solidly vs camelot math leaf.
    pub variant: DexVariant,
}

/// The Solidly family's gate projection. Solidly volatile is the
/// constant-product family → the fee-agnostic V2 lines. Solidly stable is
/// concave in the input, so its entry tangent is a sound rise beside the
/// reserve cap.
pub(crate) fn solidly_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::SolidlyStable { state } => {
            if state.stable {
                // Reserves stay NATIVE and un-oriented; the arm orients
                // them from `token_in` (the leaf's own convention).
                Some(HopMath::SolidlyStable {
                    reserves_0: state.reserves_0,
                    reserves_1: state.reserves_1,
                    decimals_0: state.decimals_0,
                    decimals_1: state.decimals_1,
                    token_in: state.token_in,
                    fee_numer: state.fee_numer,
                    fee_denom: state.fee_denom,
                })
            } else {
                let (reserve_in, reserve_out) = if state.token_in == 0 {
                    (state.reserves_0, state.reserves_1)
                } else {
                    (state.reserves_1, state.reserves_0)
                };
                Some(HopMath::SolidlyVolatile {
                    reserve_in,
                    reserve_out,
                    // Solidly stores the fee RATE (`fee_numer/fee_denom`);
                    // the Möbius tangent wants the retained fraction.
                    gamma_numer: state.fee_denom.saturating_sub(state.fee_numer),
                    fee_denom: state.fee_denom,
                })
            }
        }
        _ => None,
    }
}

/// The Solidly family's dispatch-row adapter for its single-argument path
/// entry (`SolveFn`): wraps [`solve_solidly_path_int`] and reports no walk
/// telemetry (the golden-section arm does not walk).
pub(crate) fn solve_solidly_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> crate::mixed::SolveArmOutcome {
    crate::mixed::SolveArmOutcome::walked(
        solve_solidly_path_int(resolved),
        crate::cl::WalkStats::default(),
    )
}

/// Solve an all-V2-or-Solidly (no CL hops, ≥1 Solidly) path with the shared
/// golden-section template (`solve_golden_section_path_int`).
///
/// Faithful port of `arbitrage.solvers.solidly_stable.SolidlyStableSolver`
/// (the `_solve_golden_section` branch with `swap_fn` set). The Möbius
/// precheck early-outs unprofitable paths before the expensive search.
fn solve_solidly_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;
    if hops.len() < 2 {
        return None;
    }

    // V2-equivalent IntHopState per hop: Solidly hops orient reserves by
    // `token_in` and convert the fee (SolidlyHopState stores the fee
    // fraction `fee_numer/fee_denom`; IntHopState wants the retained
    // fraction `gamma_numer/fee_denom`, so `gamma_numer = fee_denom -
    // fee_numer`). V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(solidly_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(&v2_equiv, &|x| simulate_solidly_path(x, hops), &|x| {
        simulate_solidly_path_outputs(x, hops)
    })
}

/// Build the V2-equivalent [`IntHopState`] for a hop in a Solidly solve
/// path. Solidly hops orient reserves by `token_in` and convert the fee
/// (fee fraction → retained fraction); V2 hops pass through unchanged.
/// Returns `None` for any non-V2/non-Solidly hop or when the Solidly fee
/// pair overflows `u64` (the path is then unsolvable here).
fn solidly_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::SolidlyStable { state } => {
            let (reserve_in, reserve_out) = if state.token_in == 0 {
                (state.reserves_0, state.reserves_1)
            } else {
                (state.reserves_1, state.reserves_0)
            };
            // SolidlyHopState fee = fee fraction (fee_numer/fee_denom).
            // IntHopState fee = retained fraction (gamma_numer/fee_denom).
            let gamma_numer = state.fee_denom.saturating_sub(state.fee_numer);
            let gn: u64 = gamma_numer.try_into().ok()?;
            let fd: u64 = state.fee_denom.try_into().ok()?;
            Some(IntHopState::new(reserve_in, reserve_out, gn, fd))
        }
        _ => None,
    }
}

/// Simulate a Solidly-or-V2 path, returning only the final output.
/// Per-hop dispatch: Solidly hops call the `degenbot-solidly-math` leaf
/// selected by `(variant, stable)`; V2 hops reuse `IntHopState::swap`.
#[must_use]
pub fn simulate_solidly_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    simulate_solidly_path_outputs(x, hops)
        .last()
        .copied()
        .unwrap_or(U256::ZERO)
}

/// Simulate a Solidly-or-V2 path, returning per-hop outputs.
/// `hop_outputs[i]` = output of hop `i` given `hop_outputs[i-1]` as input
/// (or `x` for `i==0`). Returns `U256::ZERO` per hop once the chain breaks
/// at a zero amount (matching the Python `_simulate_mixed_path_int`'s
/// early-out).
#[must_use]
fn simulate_solidly_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::SolidlyStable { state } => simulate_solidly_hop(amount, state),
            // Non-V2/Solidly hops can't reach here — the solve dispatch
            // rejects Solidly+CL paths before calling this.
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Evaluate a single Solidly hop's output via the `degenbot-solidly-math`
/// leaf, selected by `(variant, stable)`. Returns `U256::ZERO` on
/// overflow / invalid `token_in` (the leaf's `Err` variants), matching the
/// Python `_simulate_mixed_path_int`'s defensive fallback.
fn simulate_solidly_hop(amount_in: U256, hop: &SolidlyHopState) -> U256 {
    use degenbot_math::solidly::{
        calc_exact_in_stable_camelot, calc_exact_in_stable_solidly, calc_exact_in_volatile,
    };
    use degenbot_uniswap::dex_identity::DexVariant;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    let out = match (hop.variant, hop.stable) {
        (DexVariant::AerodromeV2Stable | DexVariant::AerodromeV2Volatile, true) => {
            calc_exact_in_stable_solidly(
                amount_in,
                hop.token_in,
                hop.reserves_0,
                hop.reserves_1,
                hop.decimals_0,
                hop.decimals_1,
                hop.fee_numer,
                hop.fee_denom,
            )
        }
        (DexVariant::CamelotV2Stable | DexVariant::CamelotV2Volatile, true) => {
            calc_exact_in_stable_camelot(
                amount_in,
                hop.token_in,
                hop.reserves_0,
                hop.reserves_1,
                hop.decimals_0,
                hop.decimals_1,
                hop.fee_numer,
                hop.fee_denom,
            )
        }
        (_, false) => calc_exact_in_volatile(
            amount_in,
            hop.token_in,
            hop.reserves_0,
            hop.reserves_1,
            hop.fee_numer,
            hop.fee_denom,
        ),
        // A non-Solidly variant with `stable=true` is impossible — these
        // DEXes have no stable pool family. Falls back to zero output.
        _ => return U256::ZERO,
    };
    out.unwrap_or(U256::ZERO)
}

/// The Solidly family's line builder: volatile pools take the Möbius rises,
/// stable pools the concave-in-the-input rise beside the reserve cap.
pub(crate) fn solidly_hop_lines(
    hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    match hop {
        HopMath::SolidlyVolatile {
            reserve_in,
            reserve_out,
            gamma_numer,
            fee_denom,
        } => {
            if reserve_in.is_zero() || reserve_out.is_zero() {
                return None;
            }
            mobius_lines(*reserve_in, *reserve_out, *gamma_numer, *fee_denom)
        }
        HopMath::SolidlyStable {
            reserves_0,
            reserves_1,
            decimals_0,
            decimals_1,
            token_in,
            fee_numer,
            fee_denom,
        } => solidly_stable_lines(
            *reserves_0,
            *reserves_1,
            *decimals_0,
            *decimals_1,
            *token_in,
            *fee_numer,
            *fee_denom,
        ),
        _ => None,
    }
}

/// Build the line set for a Solidly/Aerodrome/Camelot STABLE hop:
/// `[rise, flat]`.
///
/// The deployed invariant in the 1e18-scaled domain is
/// `f(x0, y) = x0·y·(x0² + y²) = K`, with `K = f(Ra, Rb)` at the pre-trade
/// scaled reserves. An exact-in swap sets `x0 = Ra + m` (`m` the post-fee
/// scaled input) and the output is `Rb − y`.
///
/// Implicit differentiation gives the marginal output rate
/// `dy/dx0 = −y·(3x0² + y²) / (x0·(x0² + 3y²))`; writing `t = y/x0`, the
/// rate is `price(x0) = t·(3 + t²)/(1 + 3t²)`. On the level curve
/// `x0⁴·(t + t³) = K`, so `t` strictly decreases in `x0`; and
/// `d/dt[price] = 3·(t² − 1)²/(1 + 3t²)² ≥ 0`, so the rate is non-increasing
/// in the input. The output curve is therefore CONCAVE, and a concave curve
/// through the origin lies below its entry tangent (`O(x) ≤ O'(0)·x`): the
/// entry slope is a sound global rise line.
///
/// The entry slope in NATIVE token units: the leaf scales the input by
/// `ONE/decimals_in`, scales the output by `decimals_out/ONE`, and takes the
/// fee off the input (retaining `(fee_denom − fee_numer)/fee_denom`), giving
/// `price_entry · γ/fee_denom · decimals_out/decimals_in` with
/// `price_entry = Rb·(3·Ra² + Rb²) / (Ra·(Ra² + 3·Rb²))`.
///
/// The rise carries a mandatory additive allowance `a` in native output
/// units, composed as `A = a·C` so the line evaluates to
/// `a + ceil(b·x/c)`:
/// - the fee floor retains `x − ⌊x·fee_numer/fee_denom⌋ ≥ (1 − f)·x`, so the
///   scaled input can exceed its continuous value by up to `ONE/decimals_in`,
///   worth up to `price_entry·decimals_out/decimals_in` native output — the
///   dominant term, `ceil(price_entry·decimals_out/decimals_in)`;
/// - the leaf returns the FLOOR of the invariant root (and the Camelot
///   convergence probe can stop one scaled unit high), so the realized
///   output can exceed the ideal curve by a few scaled units, each
///   `decimals_out/ONE` native — allowed for with `ceil(4·decimals_out/ONE)`.
///
/// The flat reserve cap (`out ≤ reserve_out`) stays beside the rise; the
/// point-wise minimum of both is still a rigorous upper bound. The rise is
/// DROPPED (not fatal — the cap still bounds) whenever its coefficients do not
/// fit `I512`, or when the pool sits outside the range where the leaf's own
/// invariant arithmetic is exact ACROSS THE WHOLE EVALUATION WINDOW. The gate
/// sweeps the hop's lines over `x ∈ [0, reserve_out]`, but the leaf's Newton
/// runs at the POST-trade scale `x0 = Ra + x·ONE/decimals_in`, which can exceed
/// the pre-trade `Ra` by `reserve_out·ONE/decimals_in` scaled units; there the
/// leaf's `calc_d` products wrap `u256` and a wrapped Newton denominator
/// returns a garbage root that can inflate the leaf's output ABOVE the tangent.
/// So the rise is kept only where the scaled reserves clear the leaf's own
/// precision floor [`SOLIDLY_MIN_SCALED_RESERVE`] (defined in the leaf's crate
/// beside `ONE`, because it is the leaf's arithmetic limit), the pre-trade
/// `calc_k` product cannot wrap, the leaf's input upscaling `reserve_out·ONE`
/// fits `u256`, and every post-trade `calc_d` term at
/// `x0_max = Ra + reserve_out·ONE/decimals_in`, `y ≤ Rb` stays inside `u256`.
/// Returns `None` only for a zero output reserve —
/// the degenerate exit the flat cap always took.
fn solidly_stable_lines(
    reserves_0: U256,
    reserves_1: U256,
    decimals_0: U256,
    decimals_1: U256,
    token_in: u8,
    fee_numer: U256,
    fee_denom: U256,
) -> Option<(Vec<Line>, U256)> {
    let (reserve_in, reserve_out, decimals_in, decimals_out) = if token_in == 0 {
        (reserves_0, reserves_1, decimals_0, decimals_1)
    } else {
        (reserves_1, reserves_0, decimals_1, decimals_0)
    };

    // A pool holding nothing of the output token can yield nothing.
    if reserve_out.is_zero() {
        return None;
    }

    // The flat cap is always sound (`out ≤ reserve_out`: you cannot extract
    // more than the pool holds). Every failure below falls back to it.
    let flat = Line {
        a: I512::try_from(U512::from(reserve_out)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };

    match solidly_stable_rise(
        reserve_in,
        reserve_out,
        decimals_in,
        decimals_out,
        fee_numer,
        fee_denom,
    ) {
        Some(rise) => Some((vec![rise, flat], reserve_out)),
        None => Some((vec![flat], reserve_out)),
    }
}

/// The Solidly-stable entry-tangent rise line (see [`solidly_stable_lines`]);
/// `None` whenever the rise must be dropped in favour of the flat cap alone.
/// Besides the `I512`-fit and precision checks, the gate here verifies the
/// leaf's invariant arithmetic is non-wrapping over the ENTIRE evaluation
/// window `[0, reserve_out]` — not merely at the pre-trade reserves — because a
/// wrapped post-trade Newton denominator can return a garbage root above the
/// tangent (see the `calc_d` window check below).
fn solidly_stable_rise(
    reserve_in: U256,
    reserve_out: U256,
    decimals_in: U256,
    decimals_out: U256,
    fee_numer: U256,
    fee_denom: U256,
) -> Option<Line> {
    if decimals_in.is_zero() || decimals_out.is_zero() || fee_denom.is_zero() {
        return None;
    }
    // The Solidly leaf's 18-decimal fixed-point unit — the invariant's scaled
    // domain and the additive-slack divisor.
    let one = U512::from(degenbot_math::solidly::ONE);
    let d_in = U512::from(decimals_in);
    let d_out = U512::from(decimals_out);
    let ra = U512::from(reserve_in).checked_mul(one)?.checked_div(d_in)?;
    let rb = U512::from(reserve_out)
        .checked_mul(one)?
        .checked_div(d_out)?;
    if ra.is_zero() || rb.is_zero() {
        return None;
    }

    // Faithful-arithmetic gate. The leaf's invariant helpers divide by `ONE`
    // twice and multiply the quotients, so the rise is only trustworthy where
    // the scaled reserves clear the leaf's own precision floor
    // (`SOLIDLY_MIN_SCALED_RESERVE`, defined in the leaf's crate beside `ONE`),
    // the pre-trade `calc_k` product cannot wrap `u256`, AND the whole
    // evaluation window stays non-wrapping. The gate sweeps the hop's lines
    // over `x in [0, reserve_out]`, but the leaf's Newton runs at the
    // POST-trade scale `x0 = Ra + x·ONE/decimals_in`, which exceeds the
    // pre-trade `Ra` by up to `reserve_out·ONE/decimals_in` scaled units. In
    // that regime the leaf's `calc_d` products wrap `u256` — `(x0·x0/ONE)·x0`
    // once `x0³ >
    // ONE·u256::MAX`, `y·y` once `y > 2¹²⁸`, and `3·x0·(y·y/ONE)` past
    // `u256::MAX` — and a wrapped Newton denominator yields a garbage root
    // that can inflate the leaf's output ABOVE the tangent. The rise is
    // therefore dropped (cap-only fallback) wherever the window could wrap;
    // that may only tighten the bound, never loosen it.
    let min_scaled = U512::from(SOLIDLY_MIN_SCALED_RESERVE);
    if ra < min_scaled || rb < min_scaled {
        return None;
    }
    let u256_max = U512::from(U256::MAX);
    let k_a = ra.checked_mul(rb)?.checked_div(one)?;
    let k_b = ra
        .checked_mul(ra)?
        .checked_div(one)?
        .checked_add(rb.checked_mul(rb)?.checked_div(one)?)?;
    if k_a.checked_mul(k_b)? > u256_max {
        return None;
    }
    // The leaf's own input upscaling `amount_in · ONE` (with `amount_in <=
    // reserve_out`) must fit `u256`. The four products below are the leaf's
    // `calc_d` wrap candidates at the maximal post-trade scale.
    let out_upscaled = U512::from(reserve_out).checked_mul(one)?;
    if out_upscaled > u256_max {
        return None;
    }
    // Maximal post-trade scale (`gamma <= 1` absorbed conservatively) and the
    // monotone Newton-descent bound `y <= Rb` (the seed is `Rb`, and each step
    // strictly descends toward the root).
    let x0_max = ra.checked_add(out_upscaled.checked_div(d_in)?)?;
    let x0_sq = x0_max.checked_mul(x0_max)?;
    let rb_sq = rb.checked_mul(rb)?;
    if x0_sq > u256_max
        || x0_sq.checked_div(one)?.checked_mul(x0_max)? > u256_max
        || rb_sq > u256_max
        || U512::from(3u64)
            .checked_mul(x0_max)?
            .checked_mul(rb_sq.checked_div(one)?)?
            > u256_max
    {
        return None;
    }

    // Entry marginal rate (scaled domain, pre-trade reserves).
    let three = U512::from(3u64);
    let ra2 = ra.checked_mul(ra)?;
    let rb2 = rb.checked_mul(rb)?;
    let num_price = rb.checked_mul(ra2.checked_mul(three)?.checked_add(rb2)?)?;
    let den_price = ra.checked_mul(rb2.checked_mul(three)?.checked_add(ra2)?)?;
    if den_price.is_zero() {
        return None;
    }

    // Native-unit rise: `price · γ/fee_denom · decimals_out/decimals_in`.
    let gamma = U512::from(fee_denom.saturating_sub(fee_numer));
    let b = num_price.checked_mul(gamma)?.checked_mul(d_out)?;
    let c = den_price
        .checked_mul(U512::from(fee_denom))?
        .checked_mul(d_in)?;
    if c.is_zero() {
        return None;
    }

    // Additive allowance (see the doc comment), composed as `A = a·C`.
    let a_den = den_price.checked_mul(d_in)?;
    if a_den.is_zero() {
        return None;
    }
    let a_price = num_price
        .checked_mul(d_out)?
        .checked_add(a_den)?
        .checked_sub(U512::ONE)?
        .checked_div(a_den)?;
    let a_root = U512::from(4u64)
        .checked_mul(d_out)?
        .checked_add(one - U512::ONE)?
        .checked_div(one)?;
    let big_a = a_price.checked_add(a_root)?.checked_mul(c)?;

    Some(Line {
        a: I512::try_from(big_a).ok()?,
        b: I512::try_from(b).ok()?,
        c: I512::try_from(c).ok()?,
    })
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    use crate::mixed::solve::solve_path_with_min_profit;
    use crate::profit_envelope::{path_output_bound_at, Envelope};

    #[test]
    fn m6776w_solidly_volatile_fee_floor_counterexample_is_bounded() {
        let r0 = 502_363u64;
        let r1 = 7_772_466u64;
        let x = 32u64;
        let hop = HopMath::SolidlyVolatile {
            reserve_in: U256::from(r0),
            reserve_out: U256::from(r1),
            gamma_numer: U256::from(997u64),
            fee_denom: U256::from(1_000u64),
        };
        let bound = path_output_bound_at(
            &[Some(hop)],
            &U256::from(x),
            &crate::runtime::SolveRuntimeConfig::default(),
        );
        let state = crate::mixed::SolidlyHopState {
            reserves_0: U256::from(r0),
            reserves_1: U256::from(r1),
            decimals_0: U256::from(1_000_000u64),
            decimals_1: U256::from(1_000_000u64),
            token_in: 0,
            fee_numer: U256::from(3u64),
            fee_denom: U256::from(1_000u64),
            stable: false,
            variant: DexVariant::AerodromeV2Volatile,
        };
        let true_out = simulate_solidly_hop(U256::from(x), &state);
        assert_eq!(true_out, U256::from(495u64));
        assert!(
            bound.is_some_and(|bound| bound >= true_out),
            "Solidly volatile bound must dominate true output"
        );
    }

    /// RED before the Solidly-stable tangent: the gate cannot prune a
    /// Solidly-stable path whose reserve cap sits far above the profit floor —
    /// the reserve-cap-only line makes the bound the pool's entire holding, so
    /// every unprofitable Solidly candidate still pays a full golden-section
    /// solve. The entry tangent is far tighter, so the same path (a single
    /// hop, whose true max profit is zero) is skipped once the hop maps to
    /// `SolidlyStable`.
    #[test]
    fn solidly_stable_tangent_prunes_where_reserve_cap_cannot() {
        use crate::profit_envelope::path_profit_bound_with_floor;

        let reserve = U256::from(1_000_000_000_000_000_000_000_000u128); // 1e24
        let floor = U256::from(1_000_000_000_000_000_000u64); // 1e18
        let cfg = crate::runtime::SolveRuntimeConfig::default();

        // Today's mapping: flat reserve cap only. The bound is the whole
        // holding, far above the floor, so the gate runs the solve.
        let (cap_env, _) = path_profit_bound_with_floor(
            &[Some(HopMath::ReserveCap {
                reserve_out: reserve,
            })],
            &GateDeps::offline_with_runtime(cfg),
            floor,
        );
        let Envelope::Bound(cap_bound) = cap_env else {
            panic!("the reserve-cap bound must derive");
        };
        assert!(
            cap_bound > floor,
            "the reserve cap cannot prune at this floor (bound {cap_bound} > floor {floor})"
        );

        // The new mapping: entry tangent + cap.
        let state = crate::mixed::SolidlyHopState {
            reserves_0: reserve,
            reserves_1: reserve,
            decimals_0: U256::from(1_000_000_000_000_000_000u64),
            decimals_1: U256::from(1_000_000_000_000_000_000u64),
            token_in: 0,
            fee_numer: U256::from(3u64),
            fee_denom: U256::from(10_000u64),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        };
        let (tangent_env, _) = path_profit_bound_with_floor(
            &[Some(HopMath::SolidlyStable {
                reserves_0: state.reserves_0,
                reserves_1: state.reserves_1,
                decimals_0: state.decimals_0,
                decimals_1: state.decimals_1,
                token_in: state.token_in,
                fee_numer: state.fee_numer,
                fee_denom: state.fee_denom,
            })],
            &GateDeps::offline_with_runtime(cfg),
            floor,
        );
        let Envelope::Bound(tangent_bound) = tangent_env else {
            panic!("the tangent bound must derive");
        };
        assert!(
            tangent_bound <= floor,
            "the entry tangent must prune (bound {tangent_bound} <= floor {floor})"
        );

        // Through the real entry: skipped, with no walk simulations.
        let p = ResolvedMixedPath {
            hops: vec![ResolvedHop::SolidlyStable { state }],
            valid: true,
            state_nonces: vec![1],
            max_update_block: 0,
        };
        let r = solve_path_with_min_profit(&p, floor, &GateDeps::offline());
        assert!(
            r.result.is_none(),
            "the gate must skip the unprofitable Solidly-stable path"
        );
        assert_eq!(r.gate_stats.skipped, 1);
        assert_eq!(r.stats.sims, 0, "a skip carries no walk simulations");
    }

    proptest! {
        /// Solidly volatile = constant-product: the fee-agnostic and exact-fee
        /// rise lines plus the flat cap must dominate `calc_exact_in_volatile`
        /// for every reserve/amount.
        #[test]
        fn m6776w_solidly_volatile_bound_dominates_real_leaf(
            r0 in 1u64..10_000_000u64,
            r1 in 1u64..10_000_000u64,
            x in 0u64..10_000_000u64,
            token_in in 0u8..2u8,
        ) {
            let (rin, rout) = if token_in == 0 { (r0, r1) } else { (r1, r0) };
            let hop = HopMath::SolidlyVolatile {
                reserve_in: U256::from(rin),
                reserve_out: U256::from(rout),
                gamma_numer: U256::from(997u64),
                fee_denom: U256::from(1000u64),
            };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let state = crate::mixed::SolidlyHopState {
                reserves_0: U256::from(r0),
                reserves_1: U256::from(r1),
                decimals_0: U256::from(1_000_000u64),
                decimals_1: U256::from(1_000_000u64),
                token_in,
                fee_numer: U256::from(3u64),
                fee_denom: U256::from(1000u64),
                stable: false,
                variant: DexVariant::AerodromeV2Volatile,
            };
            let true_out = simulate_solidly_hop(U256::from(x), &state);
            prop_assert!(
                bound >= true_out,
                "solidly-volatile bound {bound} < true {true_out} (r0={r0},r1={r1},x={x},t={token_in})"
            );
        }

        /// Solidly stable (stableswap): the reserve cap must dominate the real
        /// stable leaf output for every amount (guards the cap unit).
        #[test]
        fn m6776w_solidly_stable_reserve_cap_dominates_real_leaf(
            r0 in 1u64..1_000_000_000u64,
            r1 in 1u64..1_000_000_000u64,
            x in 0u64..1_000_000_000u64,
            token_in in 0u8..2u8,
        ) {
            let reserve_out = if token_in == 0 { r1 } else { r0 };
            let hop = HopMath::ReserveCap { reserve_out: U256::from(reserve_out) };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let state = crate::mixed::SolidlyHopState {
                reserves_0: U256::from(r0),
                reserves_1: U256::from(r1),
                decimals_0: U256::from(1_000_000u64),
                decimals_1: U256::from(1_000_000u64),
                token_in,
                fee_numer: U256::from(3u64),
                fee_denom: U256::from(1000u64),
                stable: true,
                variant: DexVariant::AerodromeV2Stable,
            };
            let true_out = simulate_solidly_hop(U256::from(x), &state);
            prop_assert!(
                bound >= true_out,
                "solidly-stable cap {bound} < true {true_out} (r0={r0},r1={r1},x={x},t={token_in})"
            );
        }
    }

    // ===================================================================
    // Solidly-stable faithful-arithmetic window guard.
    // ===================================================================

    /// A pool comfortably inside the faithful-arithmetic domain keeps the
    /// entry-tangent rise; a pool whose post-trade window scale would wrap the
    /// leaf's `calc_d` products drops it and falls back to the flat cap alone.
    ///
    /// The guard is asserted on the DERIVED LINE-SET SHAPE (line count + which
    /// line survives), not on any downstream bound number: the whole point is
    /// that the rise is absent for the wide-window pool.
    #[test]
    fn solidly_stable_window_guard_drops_rise_only_past_the_wrap_threshold() {
        use alloy::primitives::aliases::I512;

        let one_18 = U256::from(1_000_000_000_000_000_000u64);
        let one_6 = U256::from(1_000_000u64);

        // Balanced 1e24 / 1e24, 18-dec: `x0_max ~ 2e24`, every `calc_d` term
        // far inside u256, so the rise is ACTIVE.
        let r_healthy = U256::from(1_000_000_000_000_000_000_000_000u128);
        let (healthy_lines, healthy_cap) = solidly_stable_lines(
            r_healthy,
            r_healthy,
            one_18,
            one_18,
            0,
            U256::from(3u64),
            U256::from(10_000u64),
        )
        .expect("the healthy pool derives");
        assert_eq!(
            healthy_lines.len(),
            2,
            "the healthy pool must carry [rise, flat]"
        );
        assert!(
            healthy_lines[0].b > I512::ZERO,
            "the first line must be the non-constant rise"
        );
        assert_eq!(healthy_lines[1].b, I512::ZERO, "the second is the flat cap");
        assert_eq!(healthy_cap, r_healthy);

        // 6-dec in-reserve ~1e12 native with an 18-dec out-reserve ~1e29 native:
        // `reserve_out·ONE/decimals_in ~ 1e41`, so `x0_max² ~ 1e82` exceeds
        // `u256::MAX (~1.16e77)` and the leaf's `calc_d` `(x0·x0/ONE)·x0` term
        // wraps. The rise must be DROPPED.
        let r_in_6 = U256::from(1_000_000_000_000u128);
        let r_out_18 = U256::from(100_000_000_000_000_000_000_000_000_000u128);
        let (wide_lines, wide_cap) = solidly_stable_lines(
            r_in_6,
            r_out_18,
            one_6,
            one_18,
            0,
            U256::from(3u64),
            U256::from(10_000u64),
        )
        .expect("the wide-window pool still derives the cap");
        assert_eq!(
            wide_lines.len(),
            1,
            "the wide-window pool must fall back to cap-ONLY"
        );
        assert_eq!(
            wide_lines[0].b,
            I512::ZERO,
            "the surviving line is the flat cap"
        );
        assert_eq!(
            wide_lines[0].a,
            I512::try_from(U512::from(r_out_18)).expect("cap fits I512"),
            "the flat cap is the output reserve"
        );
        assert_eq!(wide_cap, r_out_18);
    }
}
