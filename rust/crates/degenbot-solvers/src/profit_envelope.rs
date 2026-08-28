//! Profit-envelope gate: rigorous pre-solve upper bound on path profit.
//!
//! For each hop we derive affine lines that dominate the hop's true output
//! curve point-wise; chaining across hops composes line sets (affine ∘ affine
//! stays affine), so the path-level bound is an exact point-wise minimum of
//! affine functions evaluated by integer-only arithmetic. If the maximum of
//! `bound(x) − x` over inputs falls below `min_profit`, the active-set walk is
//! provably unprofitable and is skipped without a single simulation.
//!
//! **Soundness core** (see CONTEXT.md "Profit envelope gate"):
//! - A CL hop's output curve is concave; on ending-range piece `j` its slope
//!   never exceeds the marginal price at that piece's entry × (1−fee).
//!   Extending that entry slope linearly from the piece's cumulative
//!   `(gross_input, output)` anchor therefore dominates the curve on the whole
//!   domain (tangent-line property of concave functions; for a piecewise-linear
//!   concave function each segment's own line dominates globally).
//! - CL swaps are monotone non-decreasing, so bounds chain monotonically.
//! - Every derived coefficient is computed exactly (I512); evaluation applies
//!   CEIL division so each line can only round UP, never under-cut the curve.
//!
//! **Not** a valid bound: extending the FIRST piece's Möbius map beyond its
//! validity window (deeper later ranges can beat it). Only the entry-slope
//! envelope form is sound.
//!
//! Unsupported hop families (Solidly/Curve/Balancer) make derivation return
//! `None`; the caller must NOT skip in that case (conservative).

use alloy::primitives::{aliases::I512, U256, U512};
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::IntV3TickRangeSequence;

/// One affine upper-bound line: `y = ceil((A + B·x) / C)` with `C > 0`, `B ≥ 0`.
/// `A` may be negative (lines anchored past their own window's left edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    a: I512,
    b: I512,
    c: I512,
}

impl Line {
    /// Identity: `y = x`.
    const IDENTITY: Self = Self {
        a: I512::ZERO,
        b: I512::ONE,
        c: I512::ONE,
    };

    /// Compose `self ∘ inner`: `y = self(inner(x))`. Both are affine, so the
    /// result is exact affine algebra:
    /// `(A₁ + B₁·(A₀ + B₀·x)/C₀)/C₁ = ((A₁·C₀ + B₁·A₀) + B₁·B₀·x)/(C₀·C₁)`.
    ///
    /// M6776W overflow fix: chaining affine lines across 3+ hops with
    /// 1e24+ reserves overflows `I512` during the cross-multiplication. When
    /// exact composition overflows, both operands are **sound-reduced**
    /// (right-shifted with A/B ceiling and C flooring → the ratio can only
    /// grow, i.e. the bound gets looser, never under-cuts) to `COMPOSE_TARGET_BITS`
    /// and the exact composition is retried. Two 240-bit operands multiply to
    /// ≤ 480 bits, comfortably within `I512` (511 bits). The lossy step adds
    /// at most 1 ULP per reduction, negligible at 240-bit widths.
    fn compose(&self, inner: &Self) -> Option<Self> {
        // Fast path: exact (no reduction, no loss).
        if let Some(mut r) = self.compose_exact(inner) {
            // Cap the result for the next hop's compose (M6776W overflow fix).
            r.reduce(COMPOSE_TARGET_BITS);
            return Some(r);
        }
        // Overflow: sound-reduce both operands and retry.
        let mut s = *self;
        let mut i = *inner;
        s.reduce(COMPOSE_TARGET_BITS);
        i.reduce(COMPOSE_TARGET_BITS);
        let mut r = s.compose_exact(&i)?;
        r.reduce(COMPOSE_TARGET_BITS);
        Some(r)
    }

    /// Exact composition — the algebraic identity, no reduction. Returns
    /// `None` on `I512` overflow (the caller retries after `reduce`).
    fn compose_exact(&self, inner: &Self) -> Option<Self> {
        let c01 = self.c.checked_mul(inner.c)?;
        if c01 <= I512::ZERO {
            return None;
        }
        let a = self
            .a
            .checked_mul(inner.c)?
            .checked_add(self.b.checked_mul(inner.a)?)?;
        let b = self.b.checked_mul(inner.b)?;
        Some(Self { a, b, c: c01 })
    }

    /// Sound-reduce coefficient magnitude to ≤ `target_bits` by right-shifting
    /// all three by the same `k`. Rounding is **sound** (never under-cuts the
    /// bound): A and B are **ceil**-shifted (toward +∞ → larger ratio), C is
    /// **floor**-shifted (toward 0 → smaller denominator → larger ratio), kept
    /// ≥ 1. The error is ≤ 1 ULP at the shift width — negligible for a
    /// profitability gate at 240+ bit coefficients.
    fn reduce(&mut self, target_bits: u32) {
        let max_bits = self.a.bits().max(self.b.bits()).max(self.c.bits());
        if max_bits <= target_bits {
            return;
        }
        let k = max_bits - target_bits;
        // Guard against absurd shifts (max_bits can't exceed 511 for I512,
        // but be defensive).
        if k >= 500 {
            // Catastrophic: keep only the sign.
            self.a = if self.a > I512::ZERO {
                I512::ONE
            } else {
                I512::ZERO
            };
            self.b = if self.b > I512::ZERO {
                I512::ONE
            } else {
                I512::ZERO
            };
            self.c = I512::ONE;
            return;
        }
        let c_u512 = U512::try_from(self.c).unwrap_or(U512::MAX);
        let (na, nb, nc) = (
            ceil_shr_i512(self.a, k),
            ceil_shr_i512(self.b, k),
            I512::from_raw((c_u512 / (U512::ONE << k)).max(U512::ONE)),
        );
        self.a = na;
        self.b = nb;
        self.c = nc;
    }

    /// Point-wise value, CEIL-rounded so a line never under-reads its exact
    /// rational value. Any arithmetic overflow saturates HIGH: inflating a
    /// bound keeps it an upper bound (the gate merely skips less).
    fn eval(&self, x: &U256) -> I512 {
        let Ok(xu) = I512::try_from(U512::from(*x)) else {
            return I512::MAX;
        };
        let bx = self.b.checked_mul(xu).unwrap_or(I512::MAX);
        let n = self.a.checked_add(bx).unwrap_or(I512::MAX);
        ceil_div(n, self.c)
    }
}

/// Target coefficient width after sound-reduction: two operands of this
/// width multiply to at most `2 x COMPOSE_TARGET_BITS` bits, comfortably
/// within `I512` (511 bits). Leaves ~30 bits of headroom for the cross-term
/// sum in `compose_exact`.
const COMPOSE_TARGET_BITS: u32 = 240;

/// Right-shift an `I512` by `k` with **ceiling** rounding (toward +infinity).
/// For any sign: `ceil(v / 2^k)` -- the smallest integer `>= v / 2^k`.
/// Used by `Line::reduce` to keep A/B from under-cutting the bound.
///
/// Implemented with `U512` division rather than the `I512` shift operators:
/// `alloy::I512`'s `wrapping_shr` returns ZERO for any shift >= 256 (it
/// forwards to a 256-bit path), which previously crushed reduced lines into
/// `(1,1,1)` identity shells.
fn ceil_shr_i512(value: I512, shift: u32) -> I512 {
    if shift == 0 {
        return value;
    }
    let divisor = U512::ONE << shift;
    if value >= I512::ZERO {
        let magnitude = U512::try_from(value).unwrap_or(U512::MAX);
        let quotient = magnitude / divisor;
        let has_remainder = (magnitude % divisor) != U512::ZERO;
        I512::from_raw(if has_remainder {
            quotient + U512::ONE
        } else {
            quotient
        })
    } else {
        // value < 0: ceil(value / 2^shift) = -floor(|value| / 2^shift).
        // `twos_complement()` over Signed is only valid for negatives (it
        // yields |value| as a U512 magnitude).
        let magnitude = value.twos_complement();
        let quotient = magnitude / divisor;
        // With or without a remainder: ceil(-quotient.frac) = -quotient.
        -I512::from_raw(quotient)
    }
}

/// Ceiling division for `d > 0` (truncation-toward-zero makes negatives exact).
fn ceil_div(n: I512, d: I512) -> I512 {
    debug_assert!(d > I512::ZERO);
    if n >= I512::ZERO {
        // `n` may be I512::MAX (eval() saturates on overflow), so the +d-1
        // bump must not wrap. On overflow return I512::MAX directly: it is
        // >= any ceiling of n/d, i.e. still a rigorous upper bound.
        match n.checked_add(d - I512::ONE) {
            Some(bumped) => bumped / d,
            None => I512::MAX,
        }
    } else {
        n / d
    }
}

/// What the gate needs from one resolved hop. `None` slots (unsupported
/// families) poison the whole path: no skip without a rigorous bound.
///
/// M6776W extends the gate beyond V2/CL to the Solidly/Curve/Balancer hop
/// families. Each added variant carries a RIGOROUS upper bound proven against
/// the family's real math leaf by the proptest suite in `profit_envelope_tests`.
/// The stableswap families (Solidly stable / Curve / Balancer stable) get the
/// SOUND reserve-cap bound only — their marginal rate is non-monotone (it can
/// exceed the entry rate when the pool is imbalanced toward the input), so a
/// tangent-at-zero slope is NOT sound and the amplification-bounded peak-rate
/// derivation is a documented follow-up.
#[derive(Clone, Copy, Debug)]
pub enum HopMath<'a> {
    /// Constant-product hop (exact Möbius family: V2/Aerodrome-style state).
    V2(&'a IntHopState),
    /// Concentrated-liquidity hop (ordered tick-range sequence, swap direction).
    Cl(&'a IntV3TickRangeSequence),
    /// Solidly volatile pool (constant-product family). SOUND: identical
    /// Möbius family → the V2 rise+flat lines. The fee is taken on input,
    /// which only reduces output, so the fee-agnostic bound stays rigorous
    /// (over-estimates). Reserves are NATIVE (un-oriented) and must be
    /// flipped to the swap direction by the caller.
    SolidlyVolatile { reserve_in: U256, reserve_out: U256 },
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
    /// SOUND but LOOSE reserve-only cap for the stableswap families whose
    /// marginal-rate peak needs amplification-bounded derivation (deferred).
    /// `reserve_out` is the NATIVE output-token reserve: the most you can
    /// ever extract is the pool's entire holding. Turns a `None` (unscreened,
    /// solved unscreened) into a `Some` so provably-below-floor paths still
    /// skip — a strictly-sound conservative step that never risks skipping a
    /// profitable path.
    ReserveCap { reserve_out: U256 },
}

/// Affine lines dominating one hop's output curve, plus the hop's maximum
/// extractable output (used to cap the search domain).
#[expect(clippy::too_many_lines)]
fn hop_lines_and_cap(hop: HopMath<'_>) -> Option<(Vec<Line>, U256)> {
    match hop {
        HopMath::V2(h) => {
            let (r_in, r_out) = (h.reserve_in, h.reserve_out);
            if r_in.is_zero() || r_out.is_zero() {
                return None;
            }
            // Möbius `r_out·x/(r_in+x)` satisfies both
            // `out ≤ (r_out/r_in)·x` (since r_in+x ≥ r_in) and `out ≤ r_out`.
            // As exact fractions: y = (r_out·x)/r_in and y = r_out.
            let rise = Line {
                a: I512::ZERO,
                b: I512::try_from(U512::from(r_out)).ok()?,
                c: I512::try_from(U512::from(r_in)).ok()?,
            };
            let flat = Line {
                a: I512::try_from(U512::from(r_out)).ok()?,
                b: I512::ZERO,
                c: I512::ONE,
            };
            Some((vec![rise, flat], r_out))
        }
        HopMath::Cl(seq) => {
            if seq.ranges.is_empty() {
                return None;
            }
            // O(N) single pass: `crossings()` accumulates the per-range
            // crossing once, replacing the prior O(N²) `compute_crossing(k)`
            // per-iteration re-scan that dominated gate time on dense-tick
            // pools (e.g. 1-bps USDC/USDT with thousands of ranges).
            let crossings = seq.crossings();
            let mut lines: Vec<Line> = Vec::with_capacity(crossings.len());
            for cr in &crossings {
                // Anchor cumulative (gross_input, output) at the boundary
                // ENTERING range k: `crossings[k]` carries the sum of
                // ranges [0, k) — the anchor this tangent line needs.
                let acc_in = cr.crossing_gross_input;
                let acc_out = cr.crossing_output;
                let er = &cr.ending_range;
                let liq = er.liquidity;
                if liq == 0 {
                    // Zero-liquidity range: `computeSwapStep` with L=0
                    // consumes 0 input and produces 0 output — the price
                    // advances to the range boundary for FREE, so the swap
                    // crosses this range without capital and lands in the
                    // next range. The pool may sit with its entry price in
                    // a zero-liq gap between two initialized ticks while
                    // real liquidity lives a few ranges deeper; such a pool
                    // is perfectly swappable — just skip the tangent line
                    // (a zero-liq segment has zero width: you exit it the
                    // instant you enter, at no cost, so it contributes no
                    // output line) and advance acc so the next real range's
                    // line anchors correctly. `compute_crossing(k)` gives
                    // the cumulative cost to REACH range k regardless of k's
                    // own liquidity; since crossing the zero-liq range k adds
                    // 0 to both acc_in and acc_out, the next real range's
                    // anchor is correct.
                    continue;
                }
                let p_entry = er.sqrt_price_x96;
                if p_entry.is_zero() {
                    // Zero entry price is nonsensical (the pool would be
                    // irreversibly drained) — this is a genuine degenerate
                    // case, not an arithmetic limitation.
                    return None;
                }
                // Entry marginal rate m (out/in token units):
                //   zfo: m = P²/2¹⁹² ; !zfo: m = 2¹⁹²/P²  (P = sqrt_ratio_x96)
                // Slope = ceil-free EXACT fraction (γ_num·m / fee_denom);
                // ceil happens only at evaluation.
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
                // If this range's coefficient derivation overflows I512
                // (e.g. P is at an extreme tick — a pool pushed far from fair
                // value by a misrouted swap), skip this range's tangent line
                // and continue. This is SOUND: every tangent line of a concave
                // function is a global upper bound, so the envelope `min(lines)`
                // stays valid (just looser) with fewer lines. The solver then
                // decides whether to simulate; viability checks handle
                // directional infeasibility. An extreme price is NOT evidence
                // of infeasibility — for zero_for_one it is exactly the
                // recovery direction (price moving down from the misrouted
                // extreme). Rejecting the entire hop because one range's
                // arithmetic overflowed would discard real arbitrage.
                //
                // Advance acc (crossing is computable in U256 regardless of
                // the tangent-line overflow) so the next real range's line
                // anchors at the correct cumulative offset.
                if !d512.is_zero() && !n512.is_zero() {
                    if let (Ok(d), Ok(n), Ok(oc), Ok(ic)) = (
                        I512::try_from(d512),
                        I512::try_from(n512),
                        I512::try_from(U512::from(acc_out)),
                        I512::try_from(U512::from(acc_in)),
                    ) {
                        if let Some(a) = oc
                            .checked_mul(d)
                            .and_then(|v| n.checked_mul(ic).and_then(|ni| v.checked_sub(ni)))
                        {
                            lines.push(Line { a, b: n, c: d });
                        }
                    }
                }
            }
            // Every range was zero-liquidity → genuinely dead pool (no
            // initialized tick reachable in the swap direction produces any
            // output). Reject as degenerate so classify_cl_rejection can
            // report `all_zero_liq`.
            if lines.is_empty() {
                return None;
            }
            // Cap-tail: compute the asymptotic output of the LAST range
            // (crossing it fully with infinite input) and add it to
            // acc_out. We bypass `compute_swap_step_v3` (which takes
            // `liquidity: i128`) and compute directly in U512 to accept
            // u128::MAX liquidity — the on-chain `uint128` type whose top
            // bit (>= 2^127) overflows i128 and would reject the hop.
            //
            // The formula mirrors `exact_in_step_to_target`'s output
            // (byte-identical for i128-representable liquidity — both
            // round DOWN, matching v3-core's getAmount0Delta/getAmount1Delta
            // for the "target price reachable" branch taken by
            // `compute_swap_step_v3(... I256::MAX ...)`):
            //   zfo: output = L · (sp_entry − sp_exit) / Q96
            //   ofz: output = L · Q96 · (sp_exit − sp_entry) / (sp_entry · sp_exit)
            // Both fit comfortably in U512 for any u128 L and any uint160
            // sqrt_price; the result narrows to U256 with saturation (a cap
            // shrunk by saturation is still a sound search-domain bound —
            // the binary search just explores a smaller input range).
            // The last crossing already carries the accumulated anchor
            // (O(1) reuse — no re-scan).
            let cr_last = crossings.last()?;
            let er = cr_last.ending_range.clone();
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
            // Narrow to U256 with saturation (a saturated cap is still
            // sound — see above). Then `cap = acc_out + last_range_out`.
            let last_range_out = if last_range_out_u512 > U512::from(U256::MAX) {
                U256::MAX
            } else {
                last_range_out_u512.to::<U256>()
            };
            let cap = cr_last.crossing_output.saturating_add(last_range_out);
            Some((lines, cap))
        }
        HopMath::SolidlyVolatile {
            reserve_in,
            reserve_out,
        } => {
            if reserve_in.is_zero() || reserve_out.is_zero() {
                return None;
            }
            // Identical Möbius family — fee on input only reduces output, so
            // the fee-agnostic V2 lines (rise `r_out/r_in·x` + flat `r_out`)
            // are a rigorous point-wise upper bound.
            let rise = Line {
                a: I512::ZERO,
                b: I512::try_from(U512::from(reserve_out)).ok()?,
                c: I512::try_from(U512::from(reserve_in)).ok()?,
            };
            let flat = Line {
                a: I512::try_from(U512::from(reserve_out)).ok()?,
                b: I512::ZERO,
                c: I512::ONE,
            };
            Some((vec![rise, flat], reserve_out))
        }
        HopMath::Weighted {
            balance_in,
            balance_out,
            weight_in,
            weight_out,
            scaling_in,
            scaling_out,
        } => {
            if balance_in.is_zero()
                || balance_out.is_zero()
                || weight_out.is_zero()
                || scaling_out.is_zero()
            {
                return None;
            }
            // Slope (native units) = B_out · sf_in · w_in / (B_in · w_out · sf_out),
            // computed in U512 to avoid overflow, then narrowed to I512.
            let n512 = U512::from(balance_out)
                .saturating_mul(U512::from(scaling_in))
                .saturating_mul(U512::from(weight_in));
            let d512 = U512::from(balance_in)
                .saturating_mul(U512::from(weight_out))
                .saturating_mul(U512::from(scaling_out));
            if d512.is_zero() {
                return None;
            }
            let n = I512::try_from(n512).ok()?;
            let d = I512::try_from(d512).ok()?;
            // Cap (native) = B_out / sf_out (the asymptotic reserve).
            let cap = balance_out / scaling_out;
            let rise = Line {
                a: I512::ZERO,
                b: n,
                c: d,
            };
            let flat = Line {
                a: I512::try_from(U512::from(cap)).ok()?,
                b: I512::ZERO,
                c: I512::ONE,
            };
            Some((vec![rise, flat], cap))
        }
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

/// Classify WHY a CL hop was rejected by `hop_lines_and_cap` (M6776W
/// diagnostic). Runs only when production returned `None`, so it reports
/// the *first* range that survived the zero-liq skip but failed (zero
/// price, `compute_crossing` failure, all-zero, or `cap_tail` overflow).
#[must_use]
fn classify_cl_rejection(seq: &IntV3TickRangeSequence) -> String {
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

/// Serialize a CL hop's tick-range sequence to a JSON value for the
/// degenerate-path capture harness (M6776W). Each range carries the 8
/// primitive fields the offline replay harness needs to reconstruct an
/// `IntV3TickRangeSequence` (decimal-string big-ints, matching the
/// `HeavyClPathCapture` JSONL schema in `solver_dispatch.rs`).
fn cl_seq_to_json(seq: &IntV3TickRangeSequence) -> serde_json::Value {
    serde_json::Value::Array(
        seq.ranges
            .iter()
            .map(|r| {
                serde_json::json!({
                    "liquidity": r.liquidity.to_string(),
                    "sqrt_price_x96": r.sqrt_price_x96.to_string(),
                    "sqrt_price_lower_x96": r.sqrt_price_lower_x96.to_string(),
                    "sqrt_price_upper_x96": r.sqrt_price_upper_x96.to_string(),
                    "gamma_numer": r.gamma_numer,
                    "fee_denom": r.fee_denom,
                    "zero_for_one": r.zero_for_one,
                    "word_boundary_prices": r.word_boundary_prices
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// Env-gated degenerate-path capture: serialize the full per-hop CL range
/// state + the precise rejection reason to a JSONL file so the pool states
/// can be replayed offline for fix experimentation.
///
/// # Env vars
///
/// - `DEGENBOT_GATE_CAPTURE=1` — enable capture (default: off)
/// - `DEGENBOT_GATE_CAPTURE_OUT=<path>` — output file (default:
///   `/tmp/gate_degenerate.jsonl`)
/// - `DEGENBOT_GATE_CAPTURE_CAP=<N>` — max captured paths (default: 50)
///
/// Thread-safety: a static `Mutex<()>` serializes the open + `write_all` +
/// trailing newline so concurrent path-registration threads cannot
/// interleave their (100KB+) records mid-write. `O_APPEND` keeps the file-
/// offset update atomic at the kernel level; the userspace lock keeps the
/// data write non-interleaved across the (possibly multi-syscall) write_all.
/// Serialize-to-string happens BEFORE the lock so the critical section is
/// just the open + write.
///
/// The JSONL schema matches `HeavyClPathCapture`'s format so the existing
/// offline replay harness (
/// `degenbot-solvers/tests/profit_envelope_tests.rs` golden-capture suite)
/// can load these fixtures directly.
pub(crate) fn capture_degenerate_path(
    hops: &[Option<HopMath<'_>>],
    reject_hop_index: usize,
    reject_reason: &str,
) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static CAPTURE_COUNT: AtomicU64 = AtomicU64::new(0);
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    let max: u64 = std::env::var("DEGENBOT_GATE_CAPTURE_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    if CAPTURE_COUNT.fetch_add(1, Ordering::Relaxed) >= max {
        return;
    }
    let out_path = std::env::var("DEGENBOT_GATE_CAPTURE_OUT").map_or_else(
        |_| std::path::PathBuf::from("/tmp/gate_degenerate.jsonl"),
        std::path::PathBuf::from,
    );
    // Serialize every hop's CL ranges (V2/Solidly/Weighted/ReserveCap are
    // captured as their family name + key scalars — the off-line harness
    // reconstructs CL from ranges; V2 from (reserve_in, reserve_out); etc.).
    let hops_json: Vec<serde_json::Value> = hops
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let family = match slot {
                None => "unmapped".to_string(),
                Some(HopMath::V2(h)) => {
                    format!("v2(r_in={},r_out={})", h.reserve_in, h.reserve_out)
                }
                Some(HopMath::Cl(_)) => "cl".to_string(),
                Some(HopMath::SolidlyVolatile { reserve_in, reserve_out }) => {
                    format!("solidly_volatile(r_in={reserve_in},r_out={reserve_out})")
                }
                Some(HopMath::Weighted { balance_in, balance_out, weight_in, weight_out, .. }) => {
                    format!("weighted(b_in={balance_in},b_out={balance_out},w_in={weight_in},w_out={weight_out})")
                }
                Some(HopMath::ReserveCap { reserve_out }) => {
                    format!("reserve_cap(r_out={reserve_out})")
                }
            };
            let ranges = match slot {
                Some(HopMath::Cl(seq)) => cl_seq_to_json(seq),
                _ => serde_json::Value::Null,
            };
            serde_json::json!({
                "hop_index": i,
                "family": family,
                "ranges": ranges,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "reject_hop": reject_hop_index,
        "reject_reason": reject_reason,
        "n_hops": hops.len(),
        "hops": hops_json,
    });

    // Serialize before taking the lock so the critical section is just
    // open + write_all. Records can be 100KB+; serializing under the lock
    // would extend contention for no benefit (the Value is local to this
    // call, so the to_string is race-free without the lock).
    let serialized = doc.to_string();

    // Locked append: path-registration runs on N worker threads and the gate
    // fires concurrently for each rejected path. Without a held lock the
    // per-call OpenOptions::open + writeln! across threads interleave writes
    // and corrupt the JSONL — observed post-refactor: ~19% of records were
    // unparseable because two threads' 100KB+ writes bracketed each other
    // mid-record. The static Mutex serializes open + write_all + the trailing
    // newline so each record lands as one contiguous line. O_APPEND at the
    // kernel level keeps the file-offset update atomic; the userspace lock
    // keeps the data write non-interleaved across the (possibly multi-
    // syscall) write_all. `unwrap_or_else(into_inner)` recovers from a
    // poisoned guard (a prior panicking caller) rather than propagating —
    // capture is best-effort diagnostic, not a tripwire.
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
    else {
        return;
    };
    let _ = f.write_all(serialized.as_bytes());
    let _ = f.write_all(b"\n");
}

/// Drop dominated lines: `i` dies if some `j` is `≤ i` at BOTH domain
/// endpoints (affine ⇒ everywhere), breaking ties toward the smaller index so
/// identical lines collapse deterministically. Keeps the bound sound while
/// bounding the line count across chained hops. `upper` is the envelope
/// domain endpoint: a candidate dominated at both 0 and `upper` never
/// matters for any input in [0, upper] (two affine lines cross once).
fn prune(lines: &mut Vec<Line>, upper: U256) {
    if lines.len() < 2 {
        return;
    }
    // O(K log K) dominated-line pruning via sort + sweep.
    //
    // A line j dominates line i iff j ≤ i at BOTH x=0 and x=domain with at
    // least one strict — a 2D Pareto front. We sort by (eval(0), eval(domain))
    // and sweep for running minimum eval(domain): a line is non-dominated
    // iff no line before it in sort order has eval(domain) < its eval(domain).
    // Replaces the prior O(K²) pairwise check that dominated gate time on
    // dense-CL paths (~100k intermediate composed lines → ~8s).
    //
    // Build (key0, key1, original_index) sorted ascending in all three so
    // equal-key ties are broken by index (the original rule: j < i dominates
    // on identical keys).
    let mut indexed: Vec<([I512; 2], usize)> = (0..lines.len())
        .map(|i| {
            let l = &lines[i];
            ([l.eval(&U256::ZERO), l.eval(&upper)], i)
        })
        .collect();
    indexed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    // Sweep: running min key1. A line is kept iff its key1 strictly improves
    // (decreases) the running minimum — that places it on the Pareto front.
    let mut keep = vec![false; lines.len()];
    let mut min_key1 = I512::MAX;
    for (keys, idx) in indexed {
        if keys[1] < min_key1 {
            keep[idx] = true;
            min_key1 = keys[1];
        }
    }
    let survivors: Vec<Line> = (0..lines.len())
        .filter(|&i| keep[i])
        .map(|i| lines[i])
        .collect();
    *lines = survivors;
}

/// Gate telemetry: per-solve-cycle counters, thread-local like the walk
/// stats (rayon workers aggregate them the same way). `unsupported` counts
/// paths whose hop families lack an envelope — those are SOLVED normally,
/// never skipped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateStats {
    /// Paths whose bound was derived and compared against `min_profit`.
    pub evaluated: u64,
    /// Paths skipped because `bound < min_profit` (provably unprofitable).
    pub skipped: u64,
    /// Paths with at least one unsupported hop family (solved unscreened).
    pub unsupported: u64,
    /// Per-cause breakdown of `unsupported` (M6776W diagnostic). At most one
    /// counter advances per unsupported path (the FIRST cause early-returns).
    pub none_hop_unmapped: u64,
    pub none_degenerate: u64,
    pub none_overflow: u64,
    /// Wall-clock time (nanoseconds) spent in `path_profit_bound` for this path.
    pub duration_ns: u128,
}

thread_local! {
    pub(crate) static GATE_EVALUATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_SKIPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_UNSUPPORTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    // M6776W per-cause None breakdown (advance on the early-return path).
    pub(crate) static GATE_NONE_HOP_UNMAPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_NONE_DEGENERATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_NONE_OVERFLOW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_DURATION_NS: std::cell::Cell<u128> = const { std::cell::Cell::new(0) };
}

/// Reset all gate counters on the calling thread (call at solve-cycle start,
/// mirroring [`crate::mobius_v3_int::reset_walk_stats`]).
pub fn reset_gate_stats() {
    GATE_EVALUATED.with(|c| c.set(0));
    GATE_SKIPPED.with(|c| c.set(0));
    GATE_UNSUPPORTED.with(|c| c.set(0));
    GATE_NONE_HOP_UNMAPPED.with(|c| c.set(0));
    GATE_NONE_DEGENERATE.with(|c| c.set(0));
    GATE_NONE_OVERFLOW.with(|c| c.set(0));
    GATE_DURATION_NS.with(|c| c.set(0));
}

/// Read-and-clear the calling thread's gate counters.
#[must_use]
pub fn take_last_gate_stats() -> GateStats {
    let evaluated = GATE_EVALUATED.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let skipped = GATE_SKIPPED.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let unsupported = GATE_UNSUPPORTED.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let none_hop_unmapped = GATE_NONE_HOP_UNMAPPED.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let none_degenerate = GATE_NONE_DEGENERATE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let none_overflow = GATE_NONE_OVERFLOW.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    let duration_ns = GATE_DURATION_NS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    });
    GateStats {
        evaluated,
        skipped,
        unsupported,
        none_hop_unmapped,
        none_degenerate,
        none_overflow,
        duration_ns,
    }
}

/// Rigorous upper bound on `max_x [path_output(x) − x]`, or `None` when any
/// hop is unsupported/degenerate (callers MUST NOT skip on `None`).
#[must_use]
pub fn path_profit_bound(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    let gate_t0 = std::time::Instant::now();
    let result = path_profit_bound_inner(hops);
    GATE_DURATION_NS.with(|c| c.set(gate_t0.elapsed().as_nanos()));
    result
}

#[expect(clippy::too_many_lines)]
fn path_profit_bound_inner(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    let mut all_hops: Vec<(Vec<Line>, U256)> = Vec::with_capacity(hops.len());
    let mut xmax = U256::ZERO;
    for (hop_idx, slot) in hops.iter().enumerate() {
        let Some(hop) = slot.as_ref() else {
            GATE_NONE_HOP_UNMAPPED.with(|c| c.set(c.get() + 1));
            return None;
        };
        let Some((hop_ls, cap)) = hop_lines_and_cap(*hop) else {
            GATE_NONE_DEGENERATE.with(|c| c.set(c.get() + 1));
            // M6776W degenerate diagnostic: log the hop family + the reject
            // reason so the steady-state degenerate rate can be classified as
            // the expected shape (sparse CL with empty active range / zero
            // reserves) vs a real coverage gap. Debug-level: opt-in via
            // `RUST_LOG=degenbot_solvers::profit_envelope=debug`.
            let family = match hop {
                HopMath::V2(h) => {
                    let z = h.reserve_in.is_zero() || h.reserve_out.is_zero();
                    format!("V2(zero_reserve={z})")
                }
                HopMath::Cl(seq) => {
                    let empty = seq.ranges.is_empty();
                    let reason = classify_cl_rejection(seq);
                    format!(
                        "Cl(ranges={n},empty={empty},{reason})",
                        n = seq.ranges.len()
                    )
                }
                HopMath::SolidlyVolatile {
                    reserve_in,
                    reserve_out,
                } => {
                    format!(
                        "SolidlyVolatile(zero={})",
                        reserve_in.is_zero() || reserve_out.is_zero()
                    )
                }
                HopMath::Weighted {
                    balance_in,
                    balance_out,
                    ..
                } => {
                    format!(
                        "Weighted(zero={})",
                        balance_in.is_zero() || balance_out.is_zero()
                    )
                }
                HopMath::ReserveCap { reserve_out } => {
                    format!("ReserveCap(zero={})", reserve_out.is_zero())
                }
            };
            tracing::debug!(
                target: "degenbot_solvers::profit_envelope",
                hop_index = hop_idx,
                family = %family,
                "[gate] degenerate hop rejected (impossible to bound — solved unscreened)"
            );
            // M6776W golden capture: serialize the full per-hop state when
            // env-gated so the pool states can be replayed offline for fix
            // experimentation (DEGENBOT_GATE_CAPTURE=1).
            if std::env::var_os("DEGENBOT_GATE_CAPTURE").is_some() {
                let reason = match hop {
                    HopMath::Cl(seq) => classify_cl_rejection(seq),
                    _ => family.clone(),
                };
                capture_degenerate_path(hops, hop_idx, &reason);
            }
            return None;
        };
        if let Some(v) = xmax.checked_add(cap) {
            xmax = v;
        } else {
            GATE_NONE_OVERFLOW.with(|c| c.set(c.get() + 1));
            return None;
        }
        all_hops.push((hop_ls, cap));
    }
    // Second pass against the FULL known input domain. Every line that
    // cannot be beaten below within [0,domain] can never best at any path
    // input, so the pruning endpoint assumption holds at discard time.
    let mut lines2 = vec![Line::IDENTITY];
    let domain = xmax;
    for (hop_ls, _) in &mut all_hops {
        // Prune each hop's tangent lines BEFORE composition.
        //
        // Soundness: CL swap output is monotonically increasing (more input
        // → more output). If line A dominates line B within this hop
        // (A ≤ B at both x=0 and x=domain), then for any subsequent
        // increasing composition C, C∘A ≤ C∘B — the domination survives
        // composition. So dropping B before the product loop changes
        // nothing about the final envelope.
        //
        // Effect: collapses a 3000-line CL hop to ~50 Pareto-front survivors
        // BEFORE the product loop, turning a 3000×3000 = 9M composition into
        // 50×50 = 2500. The intermediate `next` never explodes.
        prune(hop_ls, domain);
        let mut next: Vec<Line> = Vec::with_capacity(lines2.len() * hop_ls.len());
        for outer in hop_ls {
            for inner in &lines2 {
                let Some(composed) = outer.compose(inner) else {
                    GATE_NONE_OVERFLOW.with(|c| c.set(c.get() + 1));
                    return None;
                };
                next.push(composed);
            }
        }
        prune(&mut next, domain);
        lines2 = next;
    }
    let lines = lines2;
    // Diagnostic: line explosion is the gate bottleneck on dense-CL paths.
    #[cfg(not(feature = "hotpath"))]
    if lines.len() > 200 {
        let hop_counts: Vec<usize> = all_hops.iter().map(|(ls, _)| ls.len()).collect();
        tracing::warn!(
                    target: "degenbot::solver",
        gate_lines = lines.len(),
                    gate_hop_line_counts = ?hop_counts,
                    gate_domain = %domain,
                    "[gate] composed tangent-line explosion"
                );
    }
    // Discrete concave max of f(x) = min_lines(x) − x over [0, xmax].
    if xmax.is_zero() {
        return Some(U256::ZERO);
    }
    let f = |x: &U256| -> I512 {
        let mut best = I512::MAX;
        for l in &lines {
            let v = l.eval(x);
            if v < best {
                best = v;
            }
        }
        best - I512::try_from(U512::from(*x)).unwrap_or(I512::MAX)
    };
    let one = U256::from(1u8);
    let (mut lo, mut hi) = (U256::ZERO, xmax);
    while lo < hi {
        let mid = (lo >> 1) + (hi >> 1) + ((lo & hi) & one);
        if f(&(mid + one)) > f(&mid) {
            lo = mid + one;
        } else {
            hi = mid;
        }
    }
    let best = f(&lo);
    // Rounding slack: composed reductions and I512 ceiling evaluation can
    // leave one candidate line up to ~2^-11 of the bound BELOW the true
    // curve for very deep chains (block 25826949 path 400:
    // under-cut 200M on a 7.23e13 bound). Add `bound/2048` for heavy
    // chains so the gate stays sound at the cost of a tiny skip margin.
    // Only applied when the pre-scan fallback already ran (lines > 200).
    if lines.len() > 200 {
        if let Some(b) = narrow(best) {
            return Some(b.saturating_add(b / U256::from(2048u64)));
        }
    }
    narrow(best)
}

/// Narrow a non-negative bound value; `None` on overflow (>U256::MAX is
/// treated conservatively as "no usable bound").
fn narrow(v: I512) -> Option<U256> {
    // Callers guard non-negativity; keep the check here too so a future
    // call-site mistake fails conservative (ZERO = no skip) instead of
    // returning |negative| as a bogus positive bound.
    let neg = v.is_negative();
    let mag512 = v.abs();
    let mag = mag512.as_limbs();
    if !neg && mag[4] == 0 && mag[5] == 0 && mag[6] == 0 && mag[7] == 0 {
        return Some(U256::from_limbs([mag[0], mag[1], mag[2], mag[3]]));
    }
    if neg {
        // Negative bound at the argmax is impossible for x >= 0; treat like
        // any other unusable value.
        return Some(U256::ZERO);
    }
    None
}
#[must_use]
pub fn path_output_bound_at(hops: &[Option<HopMath<'_>>], x: &U256) -> Option<U256> {
    let mut lines = vec![Line::IDENTITY];
    for slot in hops {
        let Some(hop) = slot.as_ref() else {
            GATE_NONE_HOP_UNMAPPED.with(|c| c.set(c.get() + 1));
            return None;
        };
        let (hop_ls, _cap) = hop_lines_and_cap(*hop)?;
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                let Some(composed) = outer.compose(inner) else {
                    GATE_NONE_OVERFLOW.with(|c| c.set(c.get() + 1));
                    return None;
                };
                next.push(composed);
            }
        }
        prune(&mut next, *x);
        lines = next;
    }
    let mut best = I512::MAX;
    for l in &lines {
        let v = l.eval(x);
        if v < best {
            best = v;
        }
    }
    if best <= I512::ZERO {
        return Some(U256::ZERO);
    }
    narrow(best)
}

#[cfg(test)]
#[expect(clippy::expect_used)] // tiny literals; panic on typo is the point
mod tests {
    use super::*;

    /// Regression (SU7MAE 7SI5G2): eval() saturates to I512::MAX on overflow,
    /// and ceil_div previously did a bare `n + d - 1` that overflowed on that
    /// saturated input, panicking inside register_and_solve_path at startup.
    /// Regression: composition reduction on a dense 500-bit line must
    /// preserve the affine shape (alloy's `I512` shift ops return ZERO for
    /// shifts >= 256, which previously crushed reduced lines to the
    /// `(1,1,1)` identity shell and under-cut the envelope).
    #[test]
    fn reduce_keeps_affine_shape_when_shift_exceeds_256_bits() {
        let a: I512 = "3158654831486940228423188516740367875190149316773723738492435029181034827774867279178547852892671721135467331000424527445478822465180324397056000000000".parse().expect("a");
        let b: I512 = "9840653510174908457921641032450873806794527700282449709410503024478706777789039068776376210549148611598682890993376690176000000".parse().expect("b");
        let c: I512 = "10745832692113627328695180982407976136369497445284625467693498323936026077219283601227320522382063447060113625419776000000000000".parse().expect("c");
        let (b0, c0) = (U512::from(b), U512::from(c));
        let mut l = Line { a, b, c };
        l.reduce(COMPOSE_TARGET_BITS);
        // Shape must survive reduction.
        assert!(
            l.b > I512::ONE && l.c > I512::ONE && l.b < l.c,
            "reduced line collapsed to identity: a={} b={} c={}",
            l.a,
            l.b,
            l.c
        );
        // And sound reduction must not make the slope smaller than before.
        let (b1, c1) = (U512::from(l.b), U512::from(l.c));
        assert!(b1 * c0 >= b0 * c1, "reduced slope under-cuts original");
    }

    #[test]
    fn ceil_div_saturates_on_max_numerator() {
        let d = I512::try_from(3u8).expect("3 fits");
        // Saturated input must not panic and must stay an UPPER bound:
        // result >= exact ceiling of MAX/d.
        let got = ceil_div(I512::MAX, d);
        let exact_ceil = I512::MAX / d + I512::ONE; // ceil((2^511-1)/3)
        assert!(got >= exact_ceil);
        // Ordinary values stay exact-ceiling.
        let n = I512::try_from(10i64).expect("10 fits");
        assert_eq!(ceil_div(n, d), I512::try_from(4i64).expect("4 fits"));
    }
}
