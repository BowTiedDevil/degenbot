use crate::cl::{build_cl_crossing_table, ClCrossingTable};
use crate::runtime::SolveRuntimeConfig;
use alloy::primitives::{aliases::I512, U256, U512};
use degenbot_math::solidly::SOLIDLY_MIN_SCALED_RESERVE;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};
use std::sync::Arc;

use super::gate::{record_gate, GateStats};
use super::kernel::Line;

/// What the gate needs from one resolved hop. `None` slots (unsupported
/// families) poison the whole path: no skip without a rigorous bound.
///
/// The gate extends beyond V2/CL to the Solidly/Curve/Balancer hop
/// families. Each added variant carries a RIGOROUS upper bound proven against
/// the family's real math leaf by the proptest suite in `profit_envelope_tests`.
/// The Solidly/Aerodrome/Camelot STABLE invariant is concave in the input
/// (see [`HopMath::SolidlyStable`]), so it gets an entry-tangent rise beside
/// its reserve cap; the Balancer V2 STABLE invariant is likewise concave
/// wherever its closed-form curvature test holds (see
/// [`HopMath::BalancerStable`]). The Curve stableswap family keeps the SOUND
/// reserve-cap bound only: its leaf's out-curve was MEASURED non-concave (see
/// [`HopMath::CurveStableswap`]), so an entry tangent there would under-cut a
/// real leaf and skip a profitable path.
#[derive(Clone, Debug)]
pub enum HopMath<'a> {
    /// Constant-product hop (exact Möbius family: V2/Aerodrome-style state).
    V2(&'a IntHopState),
    /// Concentrated-liquidity hop: the ordered tick-range sequence plus its
    /// carried crossing table (production: the table the resolve pass already
    /// built — never re-derived per path; tableless callers use
    /// [`HopMath::cl_derived`]).
    Cl(ClHop<'a>),
    /// Solidly volatile pool (constant-product family). SOUND: identical
    /// Möbius family → the V2 rise lines. Carrying the retained fee
    /// (`gamma_numer/fee_denom`) lets the exact-fee tangent tighten the
    /// fee-agnostic rise. Reserves are NATIVE (un-oriented) and must be
    /// flipped to the swap direction by the caller.
    SolidlyVolatile {
        reserve_in: U256,
        reserve_out: U256,
        gamma_numer: U256,
        fee_denom: U256,
    },
    /// Balancer V2 weighted pool. The output curve `1 − (1+x·sf_in/B_in)^(−w_in/w_out)`
    /// is CONCAVE in the (native) input (diminishing returns), so the tangent
    /// at x=0 — the entry marginal rate `B_out·sf_in·w_in/(B_in·w_out·sf_out)` —
    /// point-wise dominates the curve (tangent-line property of concave
    /// functions) → SOUND slope line. Plus the asymptote `B_out/sf_out` reserve
    /// cap. All inputs are NATIVE token units (balances pre-divided by their
    /// scaling factors; weights are raw 18-decimal fixed-point, the ratio is
    /// exact as `w_in·1e18/w_out` evaluated lazily via the `C` denominator).
    Weighted {
        balance_in: U256,
        balance_out: U256,
        weight_in: U256,
        weight_out: U256,
        scaling_in: U256,
        scaling_out: U256,
    },
    /// SOUND but LOOSE reserve-only cap. `reserve_out` is the NATIVE
    /// output-token reserve: the most you can ever extract is the pool's
    /// entire holding. Turns a `None` (unscreened, solved unscreened) into a
    /// `Some` so provably-below-floor paths still skip — a strictly-sound
    /// conservative step that never risks skipping a profitable path. It is
    /// also the fallback for a stableswap family whose entry tangent was
    /// MEASURED non-concave against its real leaf (Curve — see
    /// [`HopMath::CurveStableswap`]) or whose rise must be dropped.
    ReserveCap { reserve_out: U256 },

    /// Balancer V2 STABLE pool (`An·Σx + D = An·D + D^(n+1)/(n^n·Πx)` in the
    /// leaf's `amp·n/AMP_PRECISION` convention). SOUND: the output curve is
    /// CONCAVE in the upscaled input wherever the closed-form curvature test
    /// [`balancer_stable_concave_at`] holds, so the entry tangent point-wise
    /// dominates it (tangent-line property of concave functions through the
    /// origin). The rise carries the leaf's fee/scaling conversion plus a
    /// mandatory additive allowance for the leaf's own integer rounding; the
    /// flat native reserve cap stays beside it. The rise is DROPPED (cap-only —
    /// never `None`, never looser than the plain cap) on `I512` overflow, on a
    /// failed curvature test, or wherever the leaf's own arithmetic could wrap
    /// across the evaluation window.
    ///
    /// `balances` BORROWS the leaf's UPSCALED 18-decimal, BPT-skipped vector
    /// (the gate maps it in the pre-solve hot loop, so it must not clone a
    /// `Vec` per hop per candidate path) and `invariant` is its pre-computed
    /// `D`; the arm converts the slope to native units with
    /// `scaling_factor_in/out` and the retained fee fraction
    /// `(ONE − swap_fee)/ONE`.
    BalancerStable {
        amp: U256,
        balances: &'a [U256],
        token_index_in: usize,
        token_index_out: usize,
        invariant: U256,
        swap_fee: U256,
        scaling_factor_in: U256,
        scaling_factor_out: U256,
    },

    /// Curve stableswap hop. Carries ONLY the state its derived lines and its
    /// family label read: a BORROWED view of the rate-adjusted `xp` (the
    /// native reserve cap is `xp[out]·precision/rate_out`), the pairwise
    /// indices, the amplification `amp` the label reports, and the output
    /// token's `precision`/`rate_multiplier_out`. The rest of the leaf's
    /// resolve-time state — the fee, `A_PRECISION`, the coin count, the input
    /// rate, and the y/d variant discriminators — is NOT carried: no reader
    /// consumes it, and the family's entry tangent was measured non-concave
    /// (see below), so no future tangent work could consume it either. `xp` is
    /// borrowed for the same hot-loop reason as
    /// [`HopMath::BalancerStable::balances`].
    ///
    /// WHY no tangent: the Curve leaf's out-curve was MEASURED against the real
    /// `degenbot_math::curve::stableswap` leaves and found NON-CONCAVE. The
    /// y-iteration floors (`div`) and can settle a unit below the invariant's
    /// root, and `dy_raw = xp_out − y − 1` then reports MORE than the ideal
    /// curve. The Standard y-iteration overshoots the entry tangent by only 1–2
    /// native units in extreme configurations, but a `Variant0`/`Variant1`
    /// y-iteration — which omits the `A_PRECISION` divisor, and is paired with
    /// whatever D-step the pool identity names, read independently by
    /// `project_curve` — overshoots by ~1e24 native units (the pool's whole
    /// output reserve) on a pool whose co-coin is nearly empty. Counterexamples
    /// are pinned in `tests/profit_envelope_tests.rs`. An entry tangent there
    /// would be an UNSOUND rise, so the family stays on the cap.
    CurveStableswap {
        amp: U256,
        xp: &'a [U256],
        token_index_in: usize,
        token_index_out: usize,
        precision: U256,
        rate_multiplier_out: U256,
    },

    /// Solidly/Aerodrome/Camelot STABLE pool (the `x0·y·(x0² + y²) = K`
    /// invariant). SOUND: the output curve is CONCAVE in the native input —
    /// writing `t = y/x0`, the marginal rate `t·(3 + t²)/(1 + 3t²)` is
    /// non-decreasing in `t` while `t` strictly decreases along the level
    /// curve as the input grows — so the entry tangent at zero input
    /// dominates the whole curve (tangent-line property of concave functions
    /// through the origin). The rise carries a mandatory additive allowance
    /// for the leaf's integer rounding; the flat reserve cap stays beside it.
    ///
    /// Reserves and decimals are NATIVE and UN-ORIENTED (the leaf's own
    /// `token_in` convention, mirroring `mixed::SolidlyHopState`); the arm
    /// orients them. `fee_numer/fee_denom` is the leaf's fee fraction.
    SolidlyStable {
        reserves_0: U256,
        reserves_1: U256,
        decimals_0: U256,
        decimals_1: U256,
        token_in: u8,
        fee_numer: U256,
        fee_denom: U256,
    },
}

/// One CL hop's gate view: the sequence plus its crossing table.
#[derive(Clone, Debug)]
pub struct ClHop<'a> {
    pub seq: &'a IntV3TickRangeSequence,
    /// Borrowed from the resolve pass's Arc table in production; owned
    /// (built once via the production builder) in the derived convenience.
    pub crossings: std::borrow::Cow<'a, ClCrossingTable>,
}

impl<'a> HopMath<'a> {
    /// Convenience for callers that only have a sequence (tests, examples,
    /// golden-reference harnesses): builds the crossing table with the
    /// production builder and carries it. The derive cost is the caller's —
    /// live solves always pass tables already built for the walk.
    #[must_use]
    pub fn cl_derived(seq: &'a IntV3TickRangeSequence) -> Self {
        Self::Cl(ClHop {
            seq,
            crossings: std::borrow::Cow::Owned(build_cl_crossing_table(seq)),
        })
    }

    /// The family label for this hop: the degenerate-path capture row's
    /// `family` field and the gate's degenerate-hop diagnostic share this ONE
    /// shape, so a new family is labelled in exactly one place and the two
    /// sites cannot diverge.
    #[must_use]
    pub fn family_label(&self) -> String {
        match self {
            HopMath::V2(h) => format!("v2(r_in={},r_out={})", h.reserve_in, h.reserve_out),
            HopMath::Cl(_) => "cl".to_string(),
            HopMath::SolidlyVolatile {
                reserve_in,
                reserve_out,
                ..
            } => format!("solidly_volatile(r_in={reserve_in},r_out={reserve_out})"),
            HopMath::Weighted {
                balance_in,
                balance_out,
                weight_in,
                weight_out,
                ..
            } => format!(
                "weighted(b_in={balance_in},b_out={balance_out},\
                 w_in={weight_in},w_out={weight_out})"
            ),
            HopMath::ReserveCap { reserve_out } => format!("reserve_cap(r_out={reserve_out})"),
            HopMath::SolidlyStable {
                reserves_0,
                reserves_1,
                token_in,
                ..
            } => format!("solidly_stable(r0={reserves_0},r1={reserves_1},token_in={token_in})"),
            HopMath::BalancerStable {
                amp,
                balances,
                token_index_in,
                token_index_out,
                ..
            } => format!(
                "balancer_stable(amp={amp},n={},i={token_index_in},j={token_index_out})",
                balances.len()
            ),
            HopMath::CurveStableswap {
                amp,
                xp,
                token_index_in,
                token_index_out,
                ..
            } => format!(
                "curve_stableswap(amp={amp},n={},i={token_index_in},j={token_index_out})",
                xp.len()
            ),
        }
    }
}

/// Build the line set for a constant-product (Möbius) hop:
/// `[rise_feeless, rise_fee'd, flat]`.
///
/// The integer leaf retains `q = x - floor(x·fee_numer/fee_denom)`,
/// equivalently `q = ceil(γ·x/fee_denom)`. The rounding term matters: the
/// fee'd rise must bound the integer-retained amount, not just the continuous
/// retained fraction. Since `q ≤ (γ·x + fee_denom - 1) / fee_denom`, the
/// following affine line dominates the real output:
/// `(γ·x + fee_denom - 1)·r_out / (fee_denom·r_in + γ·x + fee_denom - 1)`.
/// The fee-agnostic `r_out/r_in·x` also bounds the curve because `r_in + x ≥
/// r_in`. Keeping both rises plus the flat `r_out` cap and taking the
/// point-wise minimum stays a rigorous upper bound.
///
/// The fee'd rise is dropped (not fatal) when its exact coefficients exceed
/// `I512` or its denominator is zero: the fee-agnostic rise still bounds the
/// curve, so the result merely loosens. Returns `None` when the feeless
/// coefficients do not fit — no rigorous line, caller treats it as degenerate.
fn mobius_lines(
    r_in: U256,
    r_out: U256,
    gamma_numer: U256,
    fee_denom: U256,
) -> Option<(Vec<Line>, U256)> {
    let rise_feeless = Line {
        a: I512::ZERO,
        b: I512::try_from(U512::from(r_out)).ok()?,
        c: I512::try_from(U512::from(r_in)).ok()?,
    };
    let flat = Line {
        a: I512::try_from(U512::from(r_out)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };
    let mut lines = Vec::with_capacity(3);
    lines.push(rise_feeless);
    let fee_rounding = fee_denom.saturating_sub(U256::ONE);
    let a_fee = U512::from(fee_rounding).saturating_mul(U512::from(r_out));
    let b_fee = U512::from(gamma_numer).saturating_mul(U512::from(r_out));
    let c_fee = U512::from(fee_denom)
        .saturating_mul(U512::from(r_in))
        .saturating_add(U512::from(fee_rounding));
    if !c_fee.is_zero() {
        if let (Ok(a), Ok(b), Ok(c)) = (
            I512::try_from(a_fee),
            I512::try_from(b_fee),
            I512::try_from(c_fee),
        ) {
            lines.push(Line { a, b, c });
        }
    }
    lines.push(flat);
    Some((lines, r_out))
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

/// The Balancer V2 `AMP_PRECISION = 1000` — the leaf's amplification scaling
/// (`amp` is stored pre-multiplied by it, and the invariant's `amp_times_total`
/// term is divided by it).
const BALANCER_AMP_PRECISION: u64 = 1_000;

/// Exact closed-form curvature test for a StableSwap out-curve at the ENTRY
/// point, in the family's scaled domain.
///
/// Both n-token StableSwap invariants the gate covers reduce, for a fixed
/// invariant `D` and fixed co-balances, to the same implicit out-curve
/// `y² + k·y = C/x` with `k = x + T` (`T` constant in the input-side balance
/// `x`, `C` constant in `x`). Writing `m` for the invariant's amplification
/// coefficient and `W = m·(y + k)` for its product term, the marginal rate is
/// `s(x) = y·(m·x + W)/(x·(m·y + W))`; differentiating along the curve gives
/// `ds/dx = −2·y·G/(x²·(2y + k)²)` with
/// `G = (x + y + k)² + (y − x)·(y + k)`.
///
/// The curve is therefore CONCAVE at the entry exactly when `2y + k > 0` and
/// `G ≥ 0`. `y + k = C/(x·y) > 0` for positive balances, so only the sign of
/// `G` is in question — and it is a delicate one (for an extreme imbalance the
/// two terms nearly cancel), which is why it is evaluated exactly rather than
/// bounded. `k` arrives as the rational `k_num/k_den` (`k_den > 0`) so no
/// division happens; `None` means the exact evaluation overflowed `I512`, and
/// the caller drops the rise (cap-only — never looser than the plain cap).
fn stableswap_concave_at(x: U512, y: U512, k_num: U512, k_den: U512) -> Option<bool> {
    let kd = I512::try_from(k_den).ok()?;
    if kd <= I512::ZERO {
        return None;
    }
    let k = I512::try_from(k_num).ok()?;
    let x = I512::try_from(x).ok()?;
    let y = I512::try_from(y).ok()?;
    let two = I512::try_from(U512::from(2u64)).ok()?;
    let three = I512::try_from(U512::from(3u64)).ok()?;

    // `2y + k > 0` is required for a positive marginal rate.
    let two_y_plus_k = two.checked_mul(y)?.checked_mul(kd)?.checked_add(k)?;
    if two_y_plus_k <= I512::ZERO {
        return Some(false);
    }
    let x_plus_y_plus_k = x.checked_add(y)?.checked_mul(kd)?.checked_add(k)?;
    let y_times_y_plus_k = y.checked_mul(y.checked_mul(kd)?.checked_add(k)?)?;
    // 2·(3·(y² + y·k) + k²) + x·(2y + 3k), cleared of `kd` denominators.
    let curvature_term = two
        .checked_mul(
            three
                .checked_mul(y_times_y_plus_k)?
                .checked_mul(kd)?
                .checked_add(k.checked_mul(k)?)?,
        )?
        .checked_add(
            x.checked_mul(
                two.checked_mul(y)?
                    .checked_mul(kd)?
                    .checked_add(three.checked_mul(k)?)?,
            )?
            .checked_mul(kd)?,
        )?;
    // `ds/dx ≤ 0` ⟺ `2·x·(2y + k)²·kd − (x + y + k)·curvature_term ≤ 0` (the
    // common `y > 0` factor divided out; `kd > 0`).
    let lhs = two
        .checked_mul(x)?
        .checked_mul(two_y_plus_k)?
        .checked_mul(two_y_plus_k)?
        .checked_mul(kd)?;
    let rhs = x_plus_y_plus_k.checked_mul(curvature_term)?;
    Some(lhs <= rhs)
}

/// Build the line set for a Balancer V2 STABLE hop: `[rise, flat]`.
///
/// The leaf's invariant is `An·Σx + D = An·D + D^(n+1)/(n^n·Πx)` in the
/// `An = amp·n/AMP_PRECISION` convention (both `calculate_invariant` and the
/// deployed variant's Newton fixed point reduce to it). An exact-in swap adds
/// the post-fee input to the input balance and re-solves the output balance
/// against the SAME `D`, so the out-curve is that invariant's level curve. Its
/// entry marginal rate is closed-form (no invariant solve): writing
/// `W = D^(n+1)/(n^n·Πx) = m·(Σx − D) + D` and `u = x_in·x_out`,
/// `−dx_out/dx_in = (m·u + W·x_out)/(m·u + W·x_in)`.
///
/// The NATIVE-unit rise is that rate times the retained fee fraction and the
/// scaling ratio, `slope·sf_in·(ONE − swap_fee)/(ONE·sf_out)` — the leaf
/// upscales the input by `sf_in`, takes `mul_up(·, swap_fee)` off it, and
/// downscales the output by `sf_out`.
///
/// ADDITIVE ALLOWANCE: the rise carries `a = 0` — and that is derived, not
/// assumed. Every rounding in the Balancer leaf is pool-favoring: the fee is
/// taken with `mul_up` (so the retained input is at most the continuous
/// `x·(1 − f)`), the output Newton divides with `div_up` (so its root is at
/// least the invariant's), `calc_out_given_in` subtracts a further `1`, and the
/// final downscale floors. The leaf's integer output is therefore never above
/// the ideal curve, so the entry tangent dominates it with nothing to absorb
/// (measured margin `≥ 1` native unit on the proptest matrix).
///
/// The rise is DROPPED (cap-only) whenever the curvature test fails, the `I512`
/// coefficients overflow, or the leaf's arithmetic could wrap across the whole
/// evaluation window `[0, reserve_out]`: the gate sweeps the line at any native
/// input, and the leaf runs its Newton at the POST-trade scale
/// `x_in + x·sf_in`, where `D²`/`D·P_D` products can wrap `u256` and return a
/// garbage root that could exceed the tangent. The window test bounds the
/// upscaled input by `reserve_out·sf_in`, which is exactly the largest amount
/// the cap lets the gate ask for.
#[expect(clippy::too_many_arguments)] // mirrors the hop state's field set
fn balancer_stable_lines(
    amp: U256,
    balances: &[U256],
    token_index_in: usize,
    token_index_out: usize,
    invariant: U256,
    swap_fee: U256,
    scaling_factor_in: U256,
    scaling_factor_out: U256,
) -> Option<(Vec<Line>, U256)> {
    let b_out = *balances.get(token_index_out)?;
    if scaling_factor_out.is_zero() {
        return None;
    }
    // Native output reserve: you can never extract more than the pool holds.
    let reserve_out = b_out / scaling_factor_out;
    if reserve_out.is_zero() {
        return None;
    }
    let flat = Line {
        a: I512::try_from(U512::from(reserve_out)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };
    match balancer_stable_rise(
        amp,
        balances,
        token_index_in,
        token_index_out,
        invariant,
        swap_fee,
        scaling_factor_in,
        scaling_factor_out,
    ) {
        Some(rise) => Some((vec![rise, flat], reserve_out)),
        None => Some((vec![flat], reserve_out)),
    }
}

/// The Balancer-stable entry-tangent rise line (see [`balancer_stable_lines`]);
/// `None` whenever the rise must be dropped in favour of the flat cap alone.
#[expect(clippy::too_many_arguments)] // mirrors the hop state's field set
fn balancer_stable_rise(
    amp: U256,
    balances: &[U256],
    token_index_in: usize,
    token_index_out: usize,
    invariant: U256,
    swap_fee: U256,
    scaling_factor_in: U256,
    scaling_factor_out: U256,
) -> Option<Line> {
    if balances.len() < 2 || scaling_factor_in.is_zero() {
        return None;
    }
    let n_tokens = U256::from(u64::try_from(balances.len()).ok()?);
    let x_in = *balances.get(token_index_in)?;
    let x_out = *balances.get(token_index_out)?;
    if x_in.is_zero() || x_out.is_zero() {
        return None;
    }
    let amp_prec = U256::from(BALANCER_AMP_PRECISION);
    // The leaf's `amp_times_total`.
    let amp_n = amp.checked_mul(n_tokens)?;
    if amp_n.is_zero() {
        return None;
    }
    let sum = balances
        .iter()
        .try_fold(U256::ZERO, |acc, b| acc.checked_add(*b))?;
    // `Σx − D` must be non-negative for the invariant relation to hold.
    let sum_minus_d = sum.checked_sub(invariant)?;

    // `w_scaled = W·AMP_PRECISION = amp·n·(Σx − D) + AMP_PRECISION·D`.
    let w_scaled = U512::from(amp_n)
        .checked_mul(U512::from(sum_minus_d))?
        .checked_add(U512::from(amp_prec).checked_mul(U512::from(invariant))?)?;
    let cross = U512::from(amp_n)
        .checked_mul(U512::from(x_in))?
        .checked_mul(U512::from(x_out))?;
    let num = cross.checked_add(w_scaled.checked_mul(U512::from(x_out))?)?;
    let den = cross.checked_add(w_scaled.checked_mul(U512::from(x_in))?)?;
    if den.is_zero() {
        return None;
    }

    // Curvature guard at the entry: `k = x_in + Σ'' + D/m − D`, carried as the
    // rational `k_num/(amp·n)`.
    let sum_minus_out = sum.checked_sub(x_out)?;
    let k_den = U512::from(amp_n);
    let k_pos = k_den.checked_mul(U512::from(sum_minus_out))?;
    let k_prec = U512::from(amp_prec).checked_mul(U512::from(invariant))?;
    let k_neg = k_den.checked_mul(U512::from(invariant))?;
    let k_num = k_pos.checked_add(k_prec)?.checked_sub(k_neg)?;
    if !stableswap_concave_at(U512::from(x_in), U512::from(x_out), k_num, k_den)? {
        return None;
    }

    // Faithful-arithmetic window guard: the leaf's Newton runs at the maximal
    // post-trade scale, and its `invariant²`, `D·P_D` and `n·D²` products must
    // stay inside `u256` there. The window's largest upscaled input is
    // `reserve_out·scaling_factor_in` (the cap is the largest native input the
    // gate ever evaluates), and the largest output balance stays below
    // `x_out + (Σx − x_out)`.
    let u256_max = U512::from(U256::MAX);
    let out_reserve_native = x_out.checked_div(scaling_factor_out)?;
    let window_in = out_reserve_native.checked_mul(scaling_factor_in)?;
    let x_in_max = U512::from(x_in).checked_add(U512::from(window_in))?;
    let invariant_wide = U512::from(invariant);
    let n_tokens_wide = U512::from(n_tokens);
    // `n·D²` (the deployed numerator's first term) and `n·x·D²` (the monorepo
    // `D_P` accumulation at the maximal scale).
    if invariant_wide
        .checked_mul(invariant_wide)?
        .checked_mul(n_tokens_wide)?
        > u256_max
        || x_in_max
            .checked_mul(invariant_wide)?
            .checked_mul(invariant_wide)?
            .checked_mul(n_tokens_wide)?
            > u256_max
    {
        return None;
    }

    // Native slope: `slope·sf_in·(ONE − swap_fee)/(ONE·sf_out)`, with `ONE`
    // the Balancer leaf's 18-decimal fixed-point unit.
    let one = U512::from(degenbot_math::balancer::constants::ONE);
    if swap_fee >= degenbot_math::balancer::constants::ONE {
        return None;
    }
    let retained = one.checked_sub(U512::from(swap_fee))?;
    let b = num
        .checked_mul(U512::from(scaling_factor_in))?
        .checked_mul(retained)?;
    let c = den
        .checked_mul(one)?
        .checked_mul(U512::from(scaling_factor_out))?;
    if c.is_zero() {
        return None;
    }
    Some(Line {
        a: I512::ZERO,
        b: I512::try_from(b).ok()?,
        c: I512::try_from(c).ok()?,
    })
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

/// The first eligible crossing's tangent: the only CL line that anchors at the
/// table's live in-range price, so it is derived per call and never cached.
enum ClHead {
    /// No crossing carried liquidity (the table is degenerate).
    Absent,
    /// The first liquid crossing's tangent, if its coefficients fit `I512`.
    Tangent(Option<Line>),
    /// A liquid crossing had a zero entry price: hard reject, no bound.
    ZeroPrice,
}

/// Locate and derive the head tangent (see [`ClHead`]).
fn cl_head(crossings: &[IntTickRangeCrossing]) -> ClHead {
    for cr in crossings {
        let er = &cr.ending_range;
        if er.liquidity == 0 {
            continue;
        }
        if er.sqrt_price_x96.is_zero() {
            return ClHead::ZeroPrice;
        }
        return ClHead::Tangent(cl_tangent_from_crossing(cr));
    }
    ClHead::Absent
}

/// One crossing's entry-slope tangent line. `None` on a zero
/// denominator/numerator or an `I512` coefficient overflow — the caller drops
/// the line, which only loosens the envelope (every tangent is a global upper
/// bound).
fn cl_tangent_from_crossing(cr: &IntTickRangeCrossing) -> Option<Line> {
    let er = &cr.ending_range;
    let p_entry = er.sqrt_price_x96;
    // Entry marginal rate m (out/in token units):
    //   zfo: m = P²/2¹⁹² ; !zfo: m = 2¹⁹²/P²  (P = sqrt_ratio_x96)
    // Slope = ceil-free EXACT fraction (γ_num·m / fee_denom); ceil happens
    // only at evaluation.
    let p_sq = U512::from(p_entry).saturating_mul(U512::from(p_entry));
    let two192 = U512::from(1u8) << 192;
    let (m_num, m_den) = if er.zero_for_one {
        (p_sq, two192)
    } else {
        (two192, p_sq)
    };
    // line: y = acc_out + (γ·m_num / (fee_denom·m_den))·(x − acc_in)
    //      = ((acc_out·D − N·acc_in) + N·x) / D
    //   with D = fee_denom·m_den, N = γ_num·m_num.
    let d512 = U512::from(er.fee_denom).saturating_mul(m_den);
    let n512 = U512::from(er.gamma_numer).saturating_mul(m_num);
    if d512.is_zero() || n512.is_zero() {
        return None;
    }
    let d = I512::try_from(d512).ok()?;
    let n = I512::try_from(n512).ok()?;
    let oc = I512::try_from(U512::from(cr.crossing_output)).ok()?;
    let ic = I512::try_from(U512::from(cr.crossing_gross_input)).ok()?;
    let a = oc
        .checked_mul(d)
        .and_then(|v| n.checked_mul(ic).and_then(|ni| v.checked_sub(ni)))?;
    Some(Line { a, b: n, c: d })
}

/// Early-select keep-indices over the eligible-crossing ordering, plus the
/// eligible count. An empty `sel` means the table is small enough that no cap
/// is applied. Membership is the existing rule — even-spacing multiples plus
/// the last entry, or capacity-ranked under `mass` — reused unchanged.
fn cl_keep_selection(
    crossings: &[IntTickRangeCrossing],
    max_tangent_lines: usize,
    mass: bool,
) -> (usize, Vec<usize>) {
    let n_keeps = crossings
        .iter()
        .filter(|cr| {
            let er = &cr.ending_range;
            er.liquidity != 0 && !er.sqrt_price_x96.is_zero()
        })
        .count();
    let mut sel: Vec<usize> = Vec::with_capacity(max_tangent_lines + 1);
    let early = n_keeps > max_tangent_lines;
    if early && mass {
        // Loop-20 mass-weighted sampling: rank keep-indices by their range's
        // input capacity (max gross in range) instead of even index spacing,
        // keeping the heaviest shelves plus the first and last (which anchor
        // the envelope at both domain endpoints). The CL tangents sit on the
        // Pareto front (increasing intercept, decreasing slope), so ORDER
        // never matters to min() — only membership.
        let mut ranked: Vec<(U512, usize)> = Vec::with_capacity(n_keeps);
        let mut kept = 0usize;
        for cr in crossings {
            let er = &cr.ending_range;
            if er.liquidity == 0 || er.sqrt_price_x96.is_zero() {
                continue;
            }
            ranked.push((U512::from(er.max_gross_input_in_range()), kept));
            kept += 1;
        }
        ranked.sort_by_key(|x| std::cmp::Reverse(x.0));
        let mut chosen: Vec<usize> = ranked.iter().take(max_tangent_lines).map(|p| p.1).collect();
        chosen.push(0usize);
        chosen.push(n_keeps - 1);
        chosen.sort_unstable();
        chosen.dedup();
        sel.extend(chosen);
    } else if early {
        let step = (n_keeps / max_tangent_lines).max(1);
        let mut idx = 0usize;
        while idx < n_keeps {
            sel.push(idx);
            idx += step;
        }
        let last = n_keeps - 1;
        if (last % step) != 0 {
            sel.push(last);
        }
    }
    (n_keeps, sel)
}

/// Pool-static CL tangent set: every sampled line after the head, plus the
/// asymptotic cap from the last crossing. A pure function of the crossing
/// table (the head alone anchors at the live price), so it is cacheable by
/// content.
#[derive(Clone, Debug)]
pub(crate) struct ClFan {
    lines: Vec<Line>,
    cap: U256,
}

/// Derive the pool-static fan (see [`ClFan`]). `None` is a hard reject (a
/// liquid crossing carried a zero entry price, or the table is empty) and is
/// never cached.
fn derive_cl_fan(crossings: &[IntTickRangeCrossing], sel: &[usize]) -> Option<ClFan> {
    let early = !sel.is_empty();
    let mut lines: Vec<Line> = Vec::with_capacity(sel.len());
    let mut keep_idx: usize = 0;
    let mut sel_i: usize = 0;
    for cr in crossings {
        let er = &cr.ending_range;
        if er.liquidity == 0 {
            continue;
        }
        if er.sqrt_price_x96.is_zero() {
            return None;
        }
        if early {
            if sel_i < sel.len() && keep_idx == sel[sel_i] {
                sel_i += 1;
            } else {
                keep_idx += 1;
                continue;
            }
        }
        // keep_idx 0 is the head, derived per call.
        if keep_idx >= 1 {
            if let Some(l) = cl_tangent_from_crossing(cr) {
                lines.push(l);
            }
        }
        keep_idx += 1;
    }
    let cap = cl_cap_tail(crossings.last()?);
    Some(ClFan { lines, cap })
}

/// Asymptotic output of the LAST range (cross it fully with infinite input),
/// added to its accumulated anchor. Mirrors `exact_in_step_to_target`'s output
/// rounding (round DOWN) while accepting u128 liquidity that overflows i128.
fn cl_cap_tail(cr_last: &IntTickRangeCrossing) -> U256 {
    let er = &cr_last.ending_range;
    let exit = if er.zero_for_one {
        er.sqrt_price_lower_x96
    } else {
        er.sqrt_price_upper_x96
    };
    let sp_entry = er.sqrt_price_x96;
    let l_u512 = U512::from(er.liquidity);
    let q96 = U512::from(1u8) << 96;
    let last_range_out_u512 = if er.zero_for_one {
        // sp_entry >= exit (price decreasing).
        let sp_diff = U512::from(sp_entry.saturating_sub(exit));
        if sp_diff.is_zero() {
            U512::ZERO
        } else {
            (l_u512 * sp_diff) / q96
        }
    } else {
        // exit >= sp_entry (price increasing).
        let sp_diff = U512::from(exit.saturating_sub(sp_entry));
        if sp_diff.is_zero() {
            U512::ZERO
        } else {
            let denom = U512::from(sp_entry) * U512::from(exit);
            if denom.is_zero() {
                U512::ZERO
            } else {
                (l_u512 * q96 * sp_diff) / denom
            }
        }
    };
    // Narrow to U256 with saturation (a saturated cap is still sound — it can
    // only shrink the search domain).
    let last_range_out = if last_range_out_u512 > U512::from(U256::MAX) {
        U256::MAX
    } else {
        last_range_out_u512.to::<U256>()
    };
    cr_last.crossing_output.saturating_add(last_range_out)
}

/// Cap a tangent set to `max_tangent_lines` by even index stride, always
/// keeping the first and last entries (the envelope anchors). Membership is
/// reused unchanged.
fn sample_tangent_lines(lines: &[Line], max_tangent_lines: usize) -> Vec<Line> {
    let step = lines.len() / max_tangent_lines;
    let mut sampled = Vec::with_capacity(max_tangent_lines + 1);
    let mut i = 0;
    while i < lines.len() {
        sampled.push(lines[i]);
        i += step.max(1);
    }
    if sampled.last() != Some(&lines[lines.len() - 1]) {
        sampled.push(lines[lines.len() - 1]);
    }
    sampled
}

/// CL hop lines + domain cap. The head tangent (live in-range price) is
/// derived per call; the pool-static fan after it is reused from `fan_cache`
/// when present, keyed by the fan's content hash and generationed by the
/// solve-cycle epoch.
fn cl_lines_and_cap(
    crossings: &[IntTickRangeCrossing],
    max_tangent_lines: usize,
    mass: bool,
    fan_cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    if crossings.is_empty() {
        return None;
    }
    let head = match cl_head(crossings) {
        ClHead::ZeroPrice => return None,
        other => other,
    };
    // The cap consumes the last crossing: with a single crossing that is the
    // head, whose entry price is live — such a table is never cached.
    let fan_cache = if crossings.len() >= 2 {
        fan_cache
    } else {
        None
    };
    // One selection pass serves both the key and the derive; it is the same
    // O(eligible) scan the uncached path always paid.
    let (n_keeps, sel) = cl_keep_selection(crossings, max_tangent_lines, mass);
    let fan = if let Some((store, epoch)) = fan_cache {
        let key = cl_fan_key(crossings, n_keeps, &sel, max_tangent_lines, mass);
        if let Some(hit) = store.get_cl_fan(epoch, &key) {
            record_gate(stats, |t| t.cl_fan_hits += 1);
            hit
        } else {
            record_gate(stats, |t| t.cl_fan_misses += 1);
            // A hard-reject fan is never cached (no poisoned entries).
            let derived = Arc::new(derive_cl_fan(crossings, &sel)?);
            store.insert_cl_fan(epoch, key, Arc::clone(&derived));
            derived
        }
    } else {
        Arc::new(derive_cl_fan(crossings, &sel)?)
    };

    let mut lines: Vec<Line> = Vec::with_capacity(1 + fan.lines.len());
    if let ClHead::Tangent(Some(l)) = head {
        lines.push(l);
    }
    lines.extend_from_slice(&fan.lines);
    if lines.len() > max_tangent_lines {
        lines = sample_tangent_lines(&lines, max_tangent_lines);
    }
    // Every range was zero-liquidity (or every tangent overflowed) → genuinely
    // dead pool. Reject as degenerate so classify_cl_rejection can report
    // `all_zero_liq`.
    if lines.is_empty() {
        return None;
    }
    Some((lines, fan.cap))
}

/// Affine lines dominating one hop's output curve, plus the hop's maximum
/// extractable output (used to cap the search domain).
pub(crate) fn hop_lines_and_cap(
    hop: HopMath<'_>,
    cfg: &SolveRuntimeConfig,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    hop_lines_and_cap_cached(hop, cfg, None, stats)
}

/// [`hop_lines_and_cap`] with the owner's pool-static CL fan cache threaded
/// in: `fan_cache` is the prefix store plus the solve-cycle epoch. `None`
/// disables reuse (offline deps and direct callers).
pub(crate) fn hop_lines_and_cap_cached(
    hop: HopMath<'_>,
    cfg: &SolveRuntimeConfig,
    fan_cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    match hop {
        HopMath::V2(h) => {
            let (r_in, r_out) = (h.reserve_in, h.reserve_out);
            if r_in.is_zero() || r_out.is_zero() {
                return None;
            }
            mobius_lines(r_in, r_out, h.gamma_numer, h.fee_denom)
        }
        HopMath::Cl(ch) => {
            let seq = ch.seq;
            // Tangent-line budget: dense CL pools emit one tangent per range,
            // and composition across two CL hops multiplies them (K² not
            // R1×R2) — so the set is sampled to `max_tangent_lines`. Keeping
            // fewer tangents only LOOSENS the envelope (every tangent is a
            // global upper bound), so the gate never skips a profitable path.
            let max_tangent_lines = cfg.max_tangent_lines.max(1);
            if seq.ranges.is_empty() {
                return None;
            }
            // Carried crossings: the table the resolve pass already built once
            // per (pool, direction) for the active-set walk — deriving it here
            // per path dominated gate time. The table rides the [ClHop]
            // descriptor; tableless callers pay their own derive via
            // HopMath::cl_derived.
            let crossings: &[IntTickRangeCrossing] = ch.crossings.as_ref();
            cl_lines_and_cap(
                crossings,
                max_tangent_lines,
                cfg.tangent_sample_by_mass,
                fan_cache,
                stats,
            )
        }
        HopMath::SolidlyVolatile {
            reserve_in,
            reserve_out,
            gamma_numer,
            fee_denom,
        } => {
            if reserve_in.is_zero() || reserve_out.is_zero() {
                return None;
            }
            mobius_lines(reserve_in, reserve_out, gamma_numer, fee_denom)
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
            reserves_0, reserves_1, decimals_0, decimals_1, token_in, fee_numer, fee_denom,
        ),
        HopMath::BalancerStable {
            amp,
            balances,
            token_index_in,
            token_index_out,
            invariant,
            swap_fee,
            scaling_factor_in,
            scaling_factor_out,
        } => balancer_stable_lines(
            amp,
            balances,
            token_index_in,
            token_index_out,
            invariant,
            swap_fee,
            scaling_factor_in,
            scaling_factor_out,
        ),
        // Curve is gated through its own variant, but the derived lines are
        // the native reserve cap alone — its entry tangent was measured
        // non-concave against the real leaf (see `curve_stableswap_lines`).
        HopMath::CurveStableswap {
            xp,
            token_index_out,
            precision,
            rate_multiplier_out,
            ..
        } => curve_stableswap_lines(xp, token_index_out, precision, rate_multiplier_out),
        HopMath::Weighted {
            balance_in,
            balance_out,
            weight_in,
            weight_out,
            scaling_in,
            scaling_out,
        } => weighted_lines(
            balance_in,
            balance_out,
            weight_in,
            weight_out,
            scaling_in,
            scaling_out,
        ),
        HopMath::ReserveCap { reserve_out } => {
            if reserve_out.is_zero() {
                return None;
            }
            // `out ≤ reserve_out` for ALL inputs (you cannot extract more
            // than the pool holds). Flat-only — sound, loose.
            let flat = Line {
                a: I512::try_from(U512::from(reserve_out)).ok()?,
                b: I512::ZERO,
                c: I512::ONE,
            };
            Some((vec![flat], reserve_out))
        }
    }
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

/// Classify WHY a CL hop was rejected by `hop_lines_and_cap` (diagnostic). Runs only when production returned `None`, so it reports
/// the *first* range that survived the zero-liq skip but failed (zero
/// price, `compute_crossing` failure, all-zero, or `cap_tail` overflow).
#[must_use]
pub(crate) fn classify_cl_rejection(seq: &IntV3TickRangeSequence) -> String {
    if seq.ranges.is_empty() {
        return "reject=empty_ranges".to_string();
    }
    let mut any_real = false;
    // O(N) single pass (same fix as `hop_lines_and_cap`).
    for (k, cr) in seq.crossings().into_iter().enumerate() {
        let er = &cr.ending_range;
        // Zero-liquidity ranges are SKIPPED by the production envelope builder
        // (crossing is free in computeSwapStep L=0), so they are not a
        // rejection reason. Skip them here; the first range that survives
        // the skip but still fails is the rejection reason.
        if er.liquidity == 0 {
            continue;
        }
        any_real = true;
        let p = er.sqrt_price_x96;
        if p.is_zero() {
            return format!("reject=zero_price@k={k}");
        }
        // Coefficient overflow is a per-range SKIP in production (the
        // tangent line is omitted, the envelope stays sound with fewer
        // lines). An extreme entry price is not a rejection reason.
    }
    if !any_real {
        // Every range was zero-liquidity — the envelope builder rejected
        // via the `lines.is_empty()` guard. No single range is at fault;
        // the whole pool is dead (no reachable initialized tick produces
        // output in this swap direction).
        return "reject=all_zero_liq".to_string();
    }
    // Rejection fired in the cap-tail (compute_swap_step or checked_add).
    "reject=cap_tail".to_string()
}

/// Prefix-composition cache (loop-8): composed lower-envelope line sets
/// between hop boundaries, keyed by a FULL-CONTENT key per hop — the CL
/// hop's whole crossing table hashed, the Möbius hop a reserves+fee hash
/// (the allocation-pointer key + endpoint-fingerprint revalidation pair is
/// retired: identical content is the common case across a block's paths,
/// and a 128-bit FNV hit is the worst-case collision). Entries are
/// generationed by the solve-cycle epoch carried in [`GateDeps`]: first
/// touch of a new epoch clears older entries, so no entry survives a block
/// boundary and no public reset exists.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum HopCacheKey {
    ClTable(u128),
    MobiusHop(u128),
}

/// FNV-1a-style 128-bit content mix for `U256` words.
pub(crate) fn content_mix_u256(mut h: u128, v: &U256) -> u128 {
    for w in v.as_limbs() {
        h = (h ^ u128::from(*w)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Full-content key of one CL hop's crossing table. Hashing the whole table
/// per path costs a few ns/entry — far below the tangent-derivation +
/// compose work the cache saves, and immune to the allocator address reuse
/// the old pointer key only partially guarded against (its endpoint
/// fingerprints could not see mid-table pool updates).
pub(crate) fn cl_table_key(crossings: &[IntTickRangeCrossing]) -> u128 {
    let mut h = 0xcbf2_9ce4_8422_2325_u128 ^ u128::try_from(crossings.len()).unwrap_or(u128::MAX);
    for cr in crossings {
        h = content_mix_u256(h, &cr.crossing_gross_input);
        h = content_mix_u256(h, &cr.crossing_output);
        let er = &cr.ending_range;
        h = content_mix_u256(h, &er.sqrt_price_x96);
        h = content_mix_u256(h, &er.sqrt_price_lower_x96);
        h = content_mix_u256(h, &er.sqrt_price_upper_x96);
        h = (h ^ er.liquidity).wrapping_mul(0x0000_0100_0000_01B3);
        h = (h
            ^ u128::from(er.gamma_numer)
            ^ u128::from(er.fee_denom)
            ^ u128::from(u64::from(er.zero_for_one)))
        .wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Static-content key for a CL hop's pool-static tangent fan. The fan is a
/// function of the SELECTED crossings only (the head anchors at the live
/// in-range price and is rebuilt per call), so the key hashes just those
/// entries' derivation inputs plus the cap-tail inputs — re-hashing the whole
/// table per path would rival the derivation it saves. `n_keeps`/`sel.len()`
/// pin the sampling shape; the stance fields keep two configs from sharing a
/// fan.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ClFanKey {
    table: u128,
    max_tangent_lines: usize,
    mass: bool,
}

/// Mix everything `cl_tangent_from_crossing` reads.
fn mix_tangent_inputs(mut h: u128, cr: &IntTickRangeCrossing) -> u128 {
    let er = &cr.ending_range;
    h = content_mix_u256(h, &cr.crossing_gross_input);
    h = content_mix_u256(h, &cr.crossing_output);
    h = content_mix_u256(h, &er.sqrt_price_x96);
    h = (h ^ er.liquidity).wrapping_mul(0x0000_0100_0000_01B3);
    (h ^ u128::from(er.gamma_numer)
        ^ u128::from(er.fee_denom)
        ^ u128::from(u64::from(er.zero_for_one)))
    .wrapping_mul(0x0000_0100_0000_01B3)
}

/// Mix everything `cl_cap_tail` reads.
fn mix_cap_inputs(mut h: u128, cr: &IntTickRangeCrossing) -> u128 {
    let er = &cr.ending_range;
    h = content_mix_u256(h, &cr.crossing_output);
    h = content_mix_u256(h, &er.sqrt_price_x96);
    h = content_mix_u256(h, &er.sqrt_price_lower_x96);
    h = content_mix_u256(h, &er.sqrt_price_upper_x96);
    (h ^ er.liquidity ^ u128::from(u64::from(er.zero_for_one))).wrapping_mul(0x0000_0100_0000_01B3)
}

/// Build the fan key from the precomputed selection (`n_keeps`, `sel`).
fn cl_fan_key(
    crossings: &[IntTickRangeCrossing],
    n_keeps: usize,
    sel: &[usize],
    max_tangent_lines: usize,
    mass: bool,
) -> ClFanKey {
    let mut h = 0xcbf2_9ce4_8422_2325_u128
        ^ u128::try_from(crossings.len()).unwrap_or(u128::MAX)
        ^ (u128::try_from(n_keeps).unwrap_or(u128::MAX) << 7)
        ^ (u128::try_from(sel.len()).unwrap_or(u128::MAX) << 33)
        ^ u128::from(u64::from(mass));
    let early = !sel.is_empty();
    let mut keep_idx = 0usize;
    let mut sel_i = 0usize;
    for cr in crossings {
        let er = &cr.ending_range;
        if er.liquidity == 0 {
            continue;
        }
        if early {
            if sel_i < sel.len() && keep_idx == sel[sel_i] {
                sel_i += 1;
            } else {
                keep_idx += 1;
                continue;
            }
        }
        // keep_idx 0 is the head (live-priced), excluded from the fan.
        if keep_idx >= 1 {
            h = mix_tangent_inputs(h, cr);
        }
        keep_idx += 1;
    }
    if let Some(last) = crossings.last() {
        h = mix_cap_inputs(h, last);
    }
    ClFanKey {
        table: h,
        max_tangent_lines,
        mass,
    }
}

struct PrefixCacheState {
    epoch: u64,
    map: std::collections::HashMap<Vec<HopCacheKey>, Vec<Line>>,
    cl_fans: std::collections::HashMap<ClFanKey, Arc<ClFan>>,
}

/// Engine-owned gate memo (C4; loop-8 origin — the former process static
/// `PREFIX_CACHE`): composed prefix line sets plus the pool-static CL tangent
/// fans. Entries are epoch-generationed: first touch of a new epoch clears
/// older entries, so no entry survives a block boundary and no public reset
/// exists. One instance per solve owner — two engines in one process no
/// longer share cache state.
pub struct PrefixCache {
    inner: std::sync::Mutex<PrefixCacheState>,
}

impl PrefixCache {
    /// An empty store (epoch 0, no entries).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(PrefixCacheState {
                epoch: 0,
                map: std::collections::HashMap::new(),
                cl_fans: std::collections::HashMap::new(),
            }),
        }
    }

    /// Cache read against `epoch`; an epoch rollover first clears the older
    /// generation. Poisoned lock → miss (never a wrong hit).
    pub(crate) fn get(&self, epoch: u64, chain: &[HopCacheKey]) -> Option<Vec<Line>> {
        match self.inner.lock() {
            Ok(mut cache) => {
                if cache.epoch != epoch {
                    cache.epoch = epoch;
                    cache.map.clear();
                    cache.cl_fans.clear();
                }
                cache.map.get(chain).cloned()
            }
            Err(_) => None,
        }
    }

    /// Cache write against `epoch` (the same rollover-on-first-touch rule).
    pub(crate) fn insert(&self, epoch: u64, chain: Vec<HopCacheKey>, lines: Vec<Line>) {
        if let Ok(mut cache) = self.inner.lock() {
            if cache.epoch != epoch {
                cache.epoch = epoch;
                cache.map.clear();
                cache.cl_fans.clear();
            }
            cache.map.insert(chain, lines);
        }
    }

    /// Pool-static CL tangent fan against `epoch` (the same rollover-on-first
    /// touch rule as the prefix map). Poisoned lock → miss.
    pub(crate) fn get_cl_fan(&self, epoch: u64, key: &ClFanKey) -> Option<Arc<ClFan>> {
        match self.inner.lock() {
            Ok(mut cache) => {
                if cache.epoch != epoch {
                    cache.epoch = epoch;
                    cache.map.clear();
                    cache.cl_fans.clear();
                }
                cache.cl_fans.get(key).cloned()
            }
            Err(_) => None,
        }
    }

    /// Pool-static CL tangent fan write against `epoch`.
    pub(crate) fn insert_cl_fan(&self, epoch: u64, key: ClFanKey, fan: Arc<ClFan>) {
        if let Ok(mut cache) = self.inner.lock() {
            if cache.epoch != epoch {
                cache.epoch = epoch;
                cache.map.clear();
                cache.cl_fans.clear();
            }
            cache.cl_fans.insert(key, fan);
        }
    }
}

impl Default for PrefixCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PrefixCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefixCache").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)] // tiny literals; panic on typo is the point
pub(crate) mod tests {
    use super::*;

    use crate::profit_envelope::gate::GateStats;
    use crate::profit_envelope::kernel::Line;
    use degenbot_pools::int_v3_hop::IntV3TickRangeHop;

    /// Width ratchet for the gate's per-hop view.
    ///
    /// The gate builds a `Vec<Option<HopMath>>` per candidate path per call, so
    /// the enum's width is a hot-loop cost: a variant that carries state no
    /// derivation and no label reads widens the whole enum for nothing. The
    /// Curve view once carried the leaf's full resolve-time state (296 bytes)
    /// before the unread fields were dropped (200). This pins the trimmed
    /// width so a future field addition has to justify itself here.
    #[test]
    fn hop_math_view_size_stays_trimmed() {
        let size = std::mem::size_of::<HopMath<'_>>();
        assert!(
            size <= 200,
            "HopMath grew to {size} bytes; it must stay at or below the trimmed width"
        );
    }

    // Test shims: the production helpers now thread a caller-owned
    // accumulator instead of a thread-local. These tests never read the
    // prune / hop-line / reference-compose counters, so the shims discard
    // into a throwaway `GateStats`; the counter-reading tests call `super::*`
    // with an explicit local accumulator.
    pub(crate) fn hop_lines_and_cap(
        hop: HopMath<'_>,
        cfg: &SolveRuntimeConfig,
    ) -> Option<(Vec<Line>, U256)> {
        super::hop_lines_and_cap(hop, cfg, &mut GateStats::default())
    }

    fn hop_lines_and_cap_cached(
        hop: HopMath<'_>,
        cfg: &SolveRuntimeConfig,
        fan_cache: Option<(&PrefixCache, u64)>,
    ) -> Option<(Vec<Line>, U256)> {
        super::hop_lines_and_cap_cached(hop, cfg, fan_cache, &mut GateStats::default())
    }

    /// C4: the prefix cache is an owner-scoped value, not a process static.
    /// Two engines in one process keep separate stores: what engine A cached
    /// is invisible to engine B; the epoch rollover still clears per store.
    #[test]
    fn prefix_cache_is_engine_owned_and_epoch_generationed() {
        let a = PrefixCache::new();
        let b = PrefixCache::new();
        let chain = vec![HopCacheKey::MobiusHop(0x1234)];
        a.insert(1, chain.clone(), vec![Line::IDENTITY]);
        // Isolation: B holds nothing.
        assert!(
            b.get(1, &chain).is_none(),
            "engine B must not see engine A's cache"
        );
        assert!(a.get(1, &chain).is_some());
        // Epoch rollover clears inside the OWNING store only.
        a.insert(2, chain.clone(), vec![Line::IDENTITY, Line::IDENTITY]);
        assert!(
            a.get(1, &chain).is_none(),
            "stale-epoch entries drop on first touch"
        );
        // Epoch 7 never touched: no carry.
        let c = PrefixCache::new();
        assert!(c.get(7, &chain).is_none());
    }

    use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;

    // ===================================================================
    // Pool-static CL tangent-fan cache.
    // ===================================================================

    fn sp_at(tick: i32) -> U256 {
        U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap_or_default())
    }

    /// Dense zero-for-one CL sequence: `n` contiguous 1-tick ranges below the
    /// anchor, all with `liq` — a fat table that trips the tangent sample cap.
    fn fat_cl_seq(n: i32, liq: u128) -> IntV3TickRangeSequence {
        let ranges = (0..n)
            .map(|i| {
                let (tick_lo, tick_hi) = (-(i + 1), -i);
                IntV3TickRangeHop {
                    liquidity: liq,
                    sqrt_price_x96: sp_at(-i),
                    sqrt_price_lower_x96: sp_at(tick_lo),
                    sqrt_price_upper_x96: sp_at(tick_hi),
                    gamma_numer: 997_000,
                    fee_denom: 1_000_000,
                    zero_for_one: true,
                    word_boundary_prices: Vec::new(),
                }
            })
            .collect();
        IntV3TickRangeSequence::new(ranges).expect("valid sequence")
    }

    /// Content hash of a derived line set (Debug bytes suffice for an equality
    /// witness; the test also asserts exact `Vec` equality).
    fn hash_lines(lines: &[Line]) -> u128 {
        let mut h = 0xcbf2_9ce4_8422_2325_u128;
        for l in lines {
            for b in format!("{l:?}").bytes() {
                h = (h ^ u128::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
        h
    }

    /// Cold derivation, a warm cache hit, and the uncached reference must all
    /// produce byte-identical line vectors + caps — sampled (fat table) and,
    /// under both sampling stances.
    #[test]
    fn cl_fan_cache_is_byte_identical_cold_warm_and_uncached() {
        let seq = fat_cl_seq(200, 1_000_000_000_000);
        let mk = || {
            HopMath::Cl(ClHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(build_cl_crossing_table(&seq)),
            })
        };
        for mass in [false, true] {
            let cfg = SolveRuntimeConfig {
                tangent_sample_by_mass: mass,
                ..SolveRuntimeConfig::default()
            };
            let (reference, reference_cap) = hop_lines_and_cap(mk(), &cfg).expect("derivable");
            assert!(
                reference.len() < seq.ranges.len(),
                "sample cap must drop most of the 200-range table"
            );

            let store = PrefixCache::new();
            let mut stats = GateStats::EMPTY;
            let (cold, cold_cap) =
                super::hop_lines_and_cap_cached(mk(), &cfg, Some((&store, 1)), &mut stats)
                    .expect("derivable");
            assert_eq!(stats.cl_fan_misses, 1, "cold must miss once");
            assert_eq!(stats.cl_fan_hits, 0);

            let mut stats = GateStats::EMPTY;
            let (warm, warm_cap) =
                super::hop_lines_and_cap_cached(mk(), &cfg, Some((&store, 1)), &mut stats)
                    .expect("derivable");
            assert_eq!(stats.cl_fan_hits, 1, "warm must hit once");
            assert_eq!(stats.cl_fan_misses, 0);

            assert_eq!(
                hash_lines(&reference),
                hash_lines(&cold),
                "cold line hash diverged (mass={mass})"
            );
            assert_eq!(
                hash_lines(&reference),
                hash_lines(&warm),
                "warm line hash diverged (mass={mass})"
            );
            assert_eq!(reference, cold, "cold lines diverged (mass={mass})");
            assert_eq!(reference, warm, "warm lines diverged (mass={mass})");
            assert_eq!(reference_cap, cold_cap);
            assert_eq!(reference_cap, warm_cap);
        }
    }

    /// The fan key excludes the head's live price: a within-range-0 price move
    /// reuses the cached fan and only rebuilds the head tangent. A tick-set
    /// change alters the key and misses.
    #[test]
    fn cl_fan_key_ignores_head_price_and_tracks_tick_set() {
        let seq = fat_cl_seq(80, 1_000_000_000_000);
        let cfg = SolveRuntimeConfig::default();
        let base = build_cl_crossing_table(&seq);
        let view = |crossings: &[IntTickRangeCrossing]| {
            HopMath::Cl(ClHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(crossings.to_vec()),
            })
        };
        let key_of = |crossings: &[IntTickRangeCrossing]| {
            let (n, sel) = cl_keep_selection(crossings, cfg.max_tangent_lines, false);
            cl_fan_key(crossings, n, &sel, cfg.max_tangent_lines, false)
        };

        let store = PrefixCache::new();
        let (base_lines, _) =
            hop_lines_and_cap_cached(view(&base), &cfg, Some((&store, 1))).expect("derivable");

        // Live price move within range 0: only crossings[0]'s entry price
        // changes (its accumulation is zero), so the fan key is unchanged.
        let mut moved = base.clone();
        let live = moved[0].ending_range.sqrt_price_x96;
        moved[0].ending_range.sqrt_price_x96 = live.saturating_sub(U256::from(1u8));
        assert_eq!(
            key_of(&base),
            key_of(&moved),
            "price move must reuse the fan key"
        );
        let mut stats = GateStats::EMPTY;
        let (moved_lines, _) =
            super::hop_lines_and_cap_cached(view(&moved), &cfg, Some((&store, 1)), &mut stats)
                .expect("derivable");
        assert_eq!(stats.cl_fan_hits, 1, "price move must hit");
        assert_eq!(stats.cl_fan_misses, 0);
        assert_ne!(
            base_lines, moved_lines,
            "the head tangent must be rebuilt from the new live price"
        );

        // Tick-set change on a SELECTED sampled entry (range 2 is in the
        // stride set for n=80 / cap=32): the fan key changes → miss.
        let mut changed = base.clone();
        changed[2].ending_range.liquidity = 0;
        assert_ne!(
            key_of(&base),
            key_of(&changed),
            "changed tick set must produce a different key"
        );
        let mut stats = GateStats::EMPTY;
        let (_changed_lines, _) =
            super::hop_lines_and_cap_cached(view(&changed), &cfg, Some((&store, 1)), &mut stats)
                .expect("derivable");
        assert_eq!(stats.cl_fan_misses, 1, "changed table must miss");
    }

    /// A hard-reject fan (zero entry price deeper in the table) must not write
    /// a cache entry, so a subsequent valid lookup cannot be poisoned.
    #[test]
    fn cl_fan_reject_does_not_poison_cache() {
        let seq = fat_cl_seq(80, 1_000_000_000_000);
        let cfg = SolveRuntimeConfig::default();
        let store = PrefixCache::new();
        let view = |crossings: &[IntTickRangeCrossing]| {
            HopMath::Cl(ClHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(crossings.to_vec()),
            })
        };
        let mut poisoned = build_cl_crossing_table(&seq);
        poisoned[1].ending_range.sqrt_price_x96 = U256::ZERO;
        let poisoned_view = view(&poisoned);
        assert!(
            hop_lines_and_cap_cached(poisoned_view, &cfg, Some((&store, 1))).is_none(),
            "zero entry price must be a hard reject"
        );

        let valid = build_cl_crossing_table(&seq);
        let mut stats = GateStats::EMPTY;
        let hit =
            super::hop_lines_and_cap_cached(view(&valid), &cfg, Some((&store, 1)), &mut stats);
        assert!(hit.is_some(), "the valid table must still derive");
        assert_eq!(stats.cl_fan_misses, 1, "nothing was cached by the reject");
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
