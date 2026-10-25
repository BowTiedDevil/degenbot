use crate::families::concentrated_liquidity::solve::{
    ConcentratedLiquidityFan, ConcentratedLiquidityFanKey, ConcentratedLiquidityHop,
};
use crate::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};
use crate::solve::concentrated_liquidity::build_concentrated_liquidity_crossing_table;
use crate::solve::runtime::SolveRuntimeConfig;
use crate::solve::HopType;
use crate::solve::{family_row_for, FamilyRow, RESERVE_CAP_ROW};
use alloy::primitives::{aliases::I512, U256, U512};
use degenbot_math::v2::IntHopState;
use std::sync::Arc;

use super::gate::GateStats;
use super::kernel::Line;

/// What the gate needs from one resolved hop. `None` slots (unsupported
/// families) poison the whole path: no skip without a rigorous bound.
///
/// The gate extends beyond V2/concentrated liquidity to the Solidly/Curve/Balancer hop
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
    /// [`HopMath::concentrated_liquidity_derived`]).
    ConcentratedLiquidity(ConcentratedLiquidityHop<'a>),
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

impl<'a> HopMath<'a> {
    /// Convenience for callers that only have a sequence (tests, examples,
    /// golden-reference harnesses): builds the crossing table with the
    /// production builder and carries it. The derive cost is the caller's —
    /// live solves always pass tables already built for the walk.
    #[must_use]
    pub fn concentrated_liquidity_derived(seq: &'a IntV3TickRangeSequence) -> Self {
        Self::ConcentratedLiquidity(ConcentratedLiquidityHop {
            seq,
            crossings: std::borrow::Cow::Owned(build_concentrated_liquidity_crossing_table(seq)),
        })
    }

    /// The dispatch row this hop's family registers (ADR-071 D3/D4) — the ONE
    /// residual `HopMath` arm site. Every arm is a one-line delegation into the
    /// family-set table (`mixed::solve::FAMILY_DISPATCH`); the `lines` builder
    /// and the family label below are read straight off the returned row.
    pub(crate) fn family_row(&self) -> &'static FamilyRow {
        match self {
            HopMath::V2(_) => family_row_for(HopType::V2),
            HopMath::ConcentratedLiquidity(_) => family_row_for(HopType::V3),
            HopMath::SolidlyVolatile { .. } | HopMath::SolidlyStable { .. } => {
                family_row_for(HopType::SolidlyStable)
            }
            HopMath::Weighted { .. } => family_row_for(HopType::BalancerWeighted),
            HopMath::BalancerStable { .. } => family_row_for(HopType::BalancerStable),
            HopMath::CurveStableswap { .. } => family_row_for(HopType::CurveStableswap),
            HopMath::ReserveCap { .. } => &RESERVE_CAP_ROW,
        }
    }

    /// The family's envelope line builder, read off this hop's dispatch row.
    /// This is the ONE residual `lines` site: the row's `lines` fn is the
    /// family's builder, so the former `hop_lines_and_cap_cached` match is now
    /// a one-line delegation.
    pub(crate) fn lines(
        self,
        cfg: &SolveRuntimeConfig,
        fan_cache: Option<(&PrefixCache, u64)>,
        stats: &mut GateStats,
    ) -> Option<(Vec<Line>, U256)> {
        (self.family_row().lines.f)(&self, cfg, fan_cache, stats)
    }

    /// The family label for this hop: the degenerate-path capture row's
    /// `family` field and the gate's degenerate-hop diagnostic share this ONE
    /// shape, so a new family is labelled in exactly one place and the two
    /// sites cannot diverge. The family token comes from the dispatch row.
    #[must_use]
    pub fn family_label(&self) -> String {
        let label = self.family_row().label;
        match self {
            HopMath::V2(h) => format!("{label}(r_in={},r_out={})", h.reserve_in, h.reserve_out),
            HopMath::ConcentratedLiquidity(_) => label.to_string(),
            HopMath::SolidlyVolatile {
                reserve_in,
                reserve_out,
                ..
            } => format!("{label}_volatile(r_in={reserve_in},r_out={reserve_out})"),
            HopMath::Weighted {
                balance_in,
                balance_out,
                weight_in,
                weight_out,
                ..
            } => format!(
                "{label}(b_in={balance_in},b_out={balance_out},\
                 w_in={weight_in},w_out={weight_out})"
            ),
            HopMath::ReserveCap { reserve_out } => format!("{label}(r_out={reserve_out})"),
            HopMath::SolidlyStable {
                reserves_0,
                reserves_1,
                token_in,
                ..
            } => format!("{label}_stable(r0={reserves_0},r1={reserves_1},token_in={token_in})"),
            HopMath::BalancerStable {
                amp,
                balances,
                token_index_in,
                token_index_out,
                ..
            } => format!(
                "{label}(amp={amp},n={},i={token_index_in},j={token_index_out})",
                balances.len()
            ),
            HopMath::CurveStableswap {
                amp,
                xp,
                token_index_in,
                token_index_out,
                ..
            } => format!(
                "{label}(amp={amp},n={},i={token_index_in},j={token_index_out})",
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
pub(crate) fn mobius_lines(
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

/// Affine lines dominating one hop's output curve, plus the hop's maximum
/// extractable output (used to cap the search domain).
pub(crate) fn hop_lines_and_cap(
    hop: HopMath<'_>,
    cfg: &SolveRuntimeConfig,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    hop_lines_and_cap_cached(hop, cfg, None, stats)
}

/// [`hop_lines_and_cap`] with the owner's pool-static concentrated liquidity fan cache threaded
/// in: `fan_cache` is the prefix store plus the solve-cycle epoch. `None`
/// disables reuse (offline deps and direct callers).
pub(crate) fn hop_lines_and_cap_cached(
    hop: HopMath<'_>,
    cfg: &SolveRuntimeConfig,
    fan_cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    hop.lines(cfg, fan_cache, stats)
}

// ---------------------------------------------------------------------------
// The shared / residual envelope line builders (ADR-071 D3)
// ---------------------------------------------------------------------------
// The per-family builders now live in their family modules
// (`mixed::families::*`). What remains here is the shared V2 constant-product
// builder, the gate-only `ReserveCap` pseudo-family, and the refusal / walkable
// stub. Each is a `lines` entry point registered on its `FAMILY_DISPATCH` row;
// `HopMath::lines` dispatches to them through the row.

/// The V2 family's line builder: `[rise_feeless, rise_fee'd, flat]`.
pub(crate) fn v2_hop_lines(
    hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    let HopMath::V2(h) = hop else {
        return None;
    };
    let (r_in, r_out) = (h.reserve_in, h.reserve_out);
    if r_in.is_zero() || r_out.is_zero() {
        return None;
    }
    mobius_lines(r_in, r_out, h.gamma_numer, h.fee_denom)
}

/// The gate-only `ReserveCap` pseudo-family's line builder: a flat line at the
/// native reserve (`out ≤ reserve_out` for ALL inputs — you cannot extract
/// more than the pool holds). Sound, loose.
pub(crate) fn reserve_cap_hop_lines(
    hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    let HopMath::ReserveCap { reserve_out } = hop else {
        return None;
    };
    if reserve_out.is_zero() {
        return None;
    }
    let flat = Line {
        a: I512::try_from(U512::from(*reserve_out)).ok()?,
        b: I512::ZERO,
        c: I512::ONE,
    };
    Some((vec![flat], *reserve_out))
}

/// The refusal / walkable rows' line builder: no rigorous bound (a `None` hop
/// view poisons the path). Never reached through a family row's `lines`.
pub(crate) fn refuse_lines(
    _hop: &HopMath<'_>,
    _cfg: &SolveRuntimeConfig,
    _fan_cache: Option<(&PrefixCache, u64)>,
    _stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    None
}

/// Classify WHY a concentrated liquidity hop was rejected by `hop_lines_and_cap` (diagnostic). Runs only when production returned `None`, so it reports
/// the *first* range that survived the zero-liq skip but failed (zero
/// price, `compute_crossing` failure, all-zero, or `cap_tail` overflow).
#[must_use]
pub(crate) fn classify_concentrated_liquidity_rejection(seq: &IntV3TickRangeSequence) -> String {
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
/// between hop boundaries, keyed by a FULL-CONTENT key per hop — the concentrated liquidity
/// hop's whole crossing table hashed, the Möbius hop a reserves+fee hash
/// (the allocation-pointer key + endpoint-fingerprint revalidation pair is
/// retired: identical content is the common case across a block's paths,
/// and a 128-bit FNV hit is the worst-case collision). Entries are
/// generationed by the solve-cycle epoch carried in [`GateDeps`]: first
/// touch of a new epoch clears older entries, so no entry survives a block
/// boundary and no public reset exists.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum HopCacheKey {
    ConcentratedLiquidityTable(u128),
    MobiusHop(u128),
}

/// FNV-1a-style 128-bit content mix for `U256` words.
pub(crate) fn content_mix_u256(mut h: u128, v: &U256) -> u128 {
    for w in v.as_limbs() {
        h = (h ^ u128::from(*w)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Full-content key of one concentrated liquidity hop's crossing table. Hashing the whole table
/// per path costs a few ns/entry — far below the tangent-derivation +
/// compose work the cache saves, and immune to the allocator address reuse
/// the old pointer key only partially guarded against (its endpoint
/// fingerprints could not see mid-table pool updates).
pub(crate) fn concentrated_liquidity_table_key(crossings: &[IntTickRangeCrossing]) -> u128 {
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

struct PrefixCacheState {
    epoch: u64,
    map: std::collections::HashMap<Vec<HopCacheKey>, Vec<Line>>,
    concentrated_liquidity_fans:
        std::collections::HashMap<ConcentratedLiquidityFanKey, Arc<ConcentratedLiquidityFan>>,
}

/// Engine-owned gate memo (C4; loop-8 origin — the former process static
/// `PREFIX_CACHE`): composed prefix line sets plus the pool-static concentrated liquidity tangent
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
                concentrated_liquidity_fans: std::collections::HashMap::new(),
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
                    cache.concentrated_liquidity_fans.clear();
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
                cache.concentrated_liquidity_fans.clear();
            }
            cache.map.insert(chain, lines);
        }
    }

    /// Pool-static concentrated liquidity tangent fan against `epoch` (the same rollover-on-first
    /// touch rule as the prefix map). Poisoned lock → miss.
    pub(crate) fn get_concentrated_liquidity_fan(
        &self,
        epoch: u64,
        key: &ConcentratedLiquidityFanKey,
    ) -> Option<Arc<ConcentratedLiquidityFan>> {
        match self.inner.lock() {
            Ok(mut cache) => {
                if cache.epoch != epoch {
                    cache.epoch = epoch;
                    cache.map.clear();
                    cache.concentrated_liquidity_fans.clear();
                }
                cache.concentrated_liquidity_fans.get(key).cloned()
            }
            Err(_) => None,
        }
    }

    /// Pool-static concentrated liquidity tangent fan write against `epoch`.
    pub(crate) fn insert_concentrated_liquidity_fan(
        &self,
        epoch: u64,
        key: ConcentratedLiquidityFanKey,
        fan: Arc<ConcentratedLiquidityFan>,
    ) {
        if let Ok(mut cache) = self.inner.lock() {
            if cache.epoch != epoch {
                cache.epoch = epoch;
                cache.map.clear();
                cache.concentrated_liquidity_fans.clear();
            }
            cache.concentrated_liquidity_fans.insert(key, fan);
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
pub(crate) mod tests {
    use super::*;

    use crate::solve::profit_envelope::gate::GateStats;
    use crate::solve::profit_envelope::kernel::Line;

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
}
