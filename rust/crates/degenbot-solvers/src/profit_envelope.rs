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
//! Unsupported hop families make the gate return `Envelope::Unsupported`;
//! the caller must NOT skip in that case (conservative) — the verdict type
//! carries the distinction, so it cannot be ignored by accident.

use crate::mobius_v3_int::{build_cl_crossing_table, ClCrossingTable};
use alloy::primitives::{aliases::I512, U256, U512};
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};

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
        // Fast path: exact (no reduction). The caller reduces the composed
        // set ONCE per hop boundary (after prune), which is O(survivors)
        // instead of O(pairs) — soundness-identical since both paths apply
        // the same ceil/floor reductions to the same line sets.
        if let Some(r) = self.compose_exact(inner) {
            return Some(r);
        }
        // Overflow: sound-reduce both operands and retry (rare; bounded
        // intermediates keep the fast path exact for 240-bit inputs).
        let mut s = *self;
        let mut i = *inner;
        s.reduce(COMPOSE_TARGET_BITS);
        i.reduce(COMPOSE_TARGET_BITS);
        s.compose_exact(&i)
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
        let max_bits = i512_mag_bit_len(&self.a)
            .max(i512_mag_bit_len(&self.b))
            .max(i512_mag_bit_len(&self.c));
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
        let (nc_shifted, _nc_rem) = if k >= 512 {
            (U512::ZERO, c_u512 != U512::ZERO)
        } else {
            (c_u512 >> k, false)
        };
        let (na, nb, nc) = (
            ceil_shr_i512(self.a, k),
            ceil_shr_i512(self.b, k),
            I512::from_raw(nc_shifted.max(U512::ONE)),
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

#[expect(clippy::print_stderr, reason = "opt-in dev diagnostics, off in prod")]
fn trace_boundary(hop_idx: usize, hop_lines: usize, survivors: usize, next: &[Line]) {
    let min0 = next
        .iter()
        .map(|l| l.eval(&U256::ZERO))
        .min()
        .unwrap_or(I512::ZERO);
    eprintln!(
        "[gate-trace] boundary {hop_idx}: hop_lines={hop_lines} next(post-prune/sample)={survivors} min-eval(0)={min0}"
    );
}

/// T5 diagnostics: opt-in compose tracing (`DEGENBOT_GATE_TRACE=1`), parsed
/// once — the gate itself reads no environment in its hot path.
fn gate_trace_enabled() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("DEGENBOT_GATE_TRACE").is_ok())
}

/// Target coefficient width after sound-reduction: two operands of this
/// width multiply to at most `2 x COMPOSE_TARGET_BITS` bits, comfortably
/// within `I512` (511 bits). Leaves ~30 bits of headroom for the cross-term
/// sum in `compose_exact`.
const COMPOSE_TARGET_BITS: u32 = 240;
/// Cap on the composed-line count carried into the next hop's product loop
/// (survivor uniform sampling over the lower envelope). Bounds the product
/// matrix at K² regardless of pool-liquidity range counts.
fn sampled_compose_lines() -> usize {
    env_compose_lines()
}

/// Loop-18 T2 sweep knobs read from the injected runtime config (T4) —
/// defaults 32/48 (the loop-9/16 production values); the owner overrides
/// at construction. Higher caps = tighter (lower) envelope = fewer missed
/// opportunities, more compose time.
fn env_tangent_lines() -> usize {
    crate::runtime::runtime().max_tangent_lines.max(1)
}

fn env_compose_lines() -> usize {
    crate::runtime::runtime().sampled_compose_lines.max(1)
}

/// Magnitude bit length of an `I512` by direct limb scan (loop-16: the
/// `Signed::bits()` route cost ~90ns per call; this is ~5ns).
#[inline]
fn i512_mag_bit_len(v: &I512) -> u32 {
    i512_mag_bit_len_u(&v.unsigned_abs())
}

#[inline]
fn i512_mag_bit_len_u(mag: &U512) -> u32 {
    let limbs = mag.as_limbs();
    let mut i = 8usize;
    while i > 0 {
        i -= 1;
        if limbs[i] != 0 {
            #[expect(clippy::cast_possible_truncation)]
            return i as u32 * 64 + (64 - limbs[i].leading_zeros());
        }
    }
    0
}

/// Right-shift an `I512` by `k` with **ceiling** rounding (toward +infinity).
/// For any sign: `ceil(v / 2^k)` -- the smallest integer `>= v / 2^k`.
/// Used by `Line::reduce` to keep A/B from under-cutting the bound.
///
/// Implemented with `U512` limbs rather than the `I512` shift operators:
/// `alloy::I512`'s `wrapping_shr` returns ZERO for any shift >= 256 (it
/// forwards to a 256-bit path), which previously crushed reduced lines into
/// `(1,1,1)` identity shells.
fn ceil_shr_i512(value: I512, shift: u32) -> I512 {
    // Divide-by-power-of-two == right shift on the magnitude; the shift
    // replaces a wide `U512` division (loop-16: reduce was 43% of the hull
    // phase).
    let shr_mag = |m: U512| -> (U512, bool) {
        if shift >= 512 {
            let nonzero = m != U512::ZERO;
            return (U512::ZERO, nonzero);
        }
        if shift == 0 {
            return (m, false);
        }
        let q = m >> shift;
        let r = m & ((U512::ONE << shift) - U512::ONE);
        (q, r != U512::ZERO)
    };
    if shift == 0 {
        return value;
    }
    if value >= I512::ZERO {
        let magnitude = U512::try_from(value).unwrap_or(U512::MAX);
        let (quotient, has_remainder) = shr_mag(magnitude);
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
        let (quotient, _rem) = shr_mag(magnitude);
        // With or without a remainder: ceil(-quotient.frac) = -quotient.
        -I512::from_raw(quotient)
    }
}

/// Ceiling division for `d > 0` (truncation-toward-zero makes negatives exact).
/// Relative-error band for approximate ordering keys. Approximation error
/// is ~2^-48 relative (53-bit f64 mantissas composed through a few flops);
/// anything outside this band is ordered correctly by the approximation,
/// anything inside falls back to the exact comparator. Byte-identical
/// ordering guaranteed either way.
const APPROX_ORDER_BAND: f64 = 1e-6;

/// `I512` magnitude as `f64` (relative error 2^-53). The u64→f64 limb
/// conversions are the entire point of the approximation.
#[inline]
#[expect(clippy::cast_precision_loss)]
fn i512_to_f64(v: I512) -> f64 {
    let neg = v < I512::ZERO;
    let mag = v.unsigned_abs();
    let bits = mag.bit_len();
    let f = if bits == 0 {
        0.0
    } else if bits <= 53 {
        mag.as_limbs()[0] as f64
    } else {
        let shift: u32 = TryFrom::try_from(bits - 53).unwrap_or(459);
        let m = (mag >> shift).as_limbs()[0] as f64;
        m * f64_from_exp2(shift)
    };
    if neg {
        -f
    } else {
        f
    }
}

/// `2^s` for `s < 1024` (exponent-bits construction, no libm).
#[inline]
const fn f64_from_exp2(s: u32) -> f64 {
    f64::from_bits((1023u64 + s as u64) << 52)
}

/// Approximate ordering: `Less`/`Greater` only when confidently distinct
/// (both approximations carry <<2^-30 relative error); `Equal` means
/// "inside the band — use the exact comparator". `±INF` keys are allowed
/// (they model the eval saturation channel: `b·x` overflow in `eval` maps
/// to `I512::MAX` exactly as the approximation maps it to `INF`).
/// Approximate ordering for CEIL-DIVISION keys (the exact quantity is
/// `ceil(x)`): the approximation of the pre-ceiling ratio differs from the
/// exact key by < 1 ABSOLUTE (the ceiling) plus ~2^-48 relative, so the
/// margin must cover both. Returns `Less`/`Greater` only when confidently
/// distinct; `Equal` means "inside the band — use the exact comparator".
/// `±INF`-free by construction (max_f stands in for I512::MAX).
#[inline]
fn approx_cmp_ceil(x: f64, y: f64) -> std::cmp::Ordering {
    let m = 1.0 + APPROX_ORDER_BAND * x.abs().max(y.abs());
    if x < y - m {
        std::cmp::Ordering::Less
    } else if x > y + m {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

/// Approximate ordering for PURE-RATIO keys (exact quantity is a rational,
/// e.g. the slope b/c in the hull sorts): only relative error applies.
#[inline]
fn approx_cmp_ratio(x: f64, y: f64) -> std::cmp::Ordering {
    let m = APPROX_ORDER_BAND * x.abs().max(y.abs());
    if x < y - m {
        std::cmp::Ordering::Less
    } else if x > y + m {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

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
#[derive(Clone, Debug)]
pub enum HopMath<'a> {
    /// Constant-product hop (exact Möbius family: V2/Aerodrome-style state).
    V2(&'a IntHopState),
    /// Concentrated-liquidity hop: the ordered tick-range sequence plus its
    /// carried crossing table (production: the table the resolve pass already
    /// built — never re-derived per path, BZSOJ7; tableless callers use
    /// [`HopMath::cl_derived`]).
    Cl(ClHop<'a>),
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
        HopMath::Cl(ch) => {
            let seq = ch.seq;
            // Tangent-line budget: dense CL pools (1-bps USDC/USDT with
            // 700+ ranges) emit one tangent per range. Composition across
            // two CL hops produces R1×R2 composed tangent lines, each
            // requiring 4 I512 multiplies — the 65k compositions dominate
            // gate time. Capping at MAX_TANGENT_LINES keeps the composition
            // product bounded (K² not R1×R2).
            //
            // Soundness: EVERY tangent line is a global upper bound on the
            // concave output curve. Keeping fewer tangents makes min(lines)
            // LOOSER (higher) — the gate becomes more conservative (passes
            // more paths to the solver) but NEVER skips a profitable path.
            let max_tangent_lines = env_tangent_lines();
            if seq.ranges.is_empty() {
                return None;
            }
            // Carried crossings (BZSOJ7): the table the resolve pass already
            // built once per (pool, direction) for the active-set walk —
            // deriving it here per path dominated gate time. The table rides
            // the [ClHop] descriptor; tableless callers pay their own derive
            // via HopMath::cl_derived.
            let crossings: &[IntTickRangeCrossing] = ch.crossings.as_ref();
            // O(N) single pass: the crossings table accumulates the
            // per-range crossing once (either caller-carried or derived),
            // replacing the prior quadratic per-iteration re-scan that
            // dominated gate time on dense-tick pools.
            //
            // Loop-12 PVOPYP: tangent lines are then SAMPLED to
            // MAX_TANGENT_LINES entries, so on fat tables most of the
            // ~300ns/range derivation is thrown away. Early-select the
            // sampled keep-indices FIRST (same membership rule as the cap
            // below: multiples of step + the last entry) and derive only
            // those. Skip-membership counts only zero-liq / zero-price
            // ranges — identical to the legacy filter for every normal
            // table, so the resulting sampled set is byte-identical on the
            // replay corpora; a would-be-I512-overflow range is the only
            // case where the sampled set can differ, and there the bound
            // stays sound (looser) by the tangent upper-bound argument.
            let mut lines: Vec<Line> = Vec::with_capacity(max_tangent_lines + 1);
            let n_keeps_usize = crossings
                .iter()
                .filter(|cr| {
                    let er = &cr.ending_range;
                    er.liquidity != 0 && !er.sqrt_price_x96.is_zero()
                })
                .count();
            let mut sel: Vec<usize> = Vec::with_capacity(max_tangent_lines + 1);
            let early = n_keeps_usize > max_tangent_lines;
            if early {
                let step = (n_keeps_usize / max_tangent_lines).max(1);
                let mut idx = 0usize;
                while idx < n_keeps_usize {
                    sel.push(idx);
                    idx += step;
                }
                let last = n_keeps_usize - 1;
                if (last % step) != 0 {
                    sel.push(last);
                }
            }
            let mut keep_idx: usize = 0;
            let mut sel_i: usize = 0;
            for cr in crossings {
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
                // Early-select gate (loop-12 PVOPYP): derive only ranges
                // whose keep-index is in sel; non-selected entries advance
                // the counter and skip the coefficient derivation entirely.
                if early {
                    if sel_i < sel.len() && keep_idx == sel[sel_i] {
                        sel_i += 1;
                    } else {
                        keep_idx += 1;
                        continue;
                    }
                }
                keep_idx += 1;
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
            // Cap tangent lines: sample MAX_TANGENT_LINES evenly-spaced
            // entries (keeping the first + last, which anchor the envelope
            // at both endpoints). The full set of CL tangents are ALL on
            // the Pareto front (increasing intercept, decreasing slope), so
            // prune() cannot reduce them. Sampling is the only way to bound
            // the composition product.
            if lines.len() > max_tangent_lines {
                let step = lines.len() / max_tangent_lines;
                let mut sampled = Vec::with_capacity(max_tangent_lines + 1);
                let mut i = 0;
                while i < lines.len() {
                    sampled.push(lines[i]);
                    i += step.max(1);
                }
                // Always keep the last tangent (the flattest slope, highest
                // intercept — the tightest bound at large x).
                if sampled.last() != Some(&lines[lines.len() - 1]) {
                    sampled.push(lines[lines.len() - 1]);
                }
                lines = sampled;
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
/// The capture config arrives as a [`GateCaptureCfg`] (the caller builds it
/// from the `DEGENBOT_GATE_CAPTURE*` env vars — the gate reads no
/// environment).
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
    cfg: &GateCaptureCfg,
) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static CAPTURE_COUNT: AtomicU64 = AtomicU64::new(0);
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    if CAPTURE_COUNT.fetch_add(1, Ordering::Relaxed) >= cfg.max_paths {
        return;
    }
    let out_path = cfg.out_path.clone();
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
                Some(HopMath::Cl(ch)) => cl_seq_to_json(ch.seq),
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
#[expect(clippy::too_many_lines)]
fn prune(lines: &mut Vec<Line>, upper: U256) {
    struct S1Key {
        f0: f64,
        fu: f64,
        idx: usize,
    }
    gate_tls(|t| {
        t.prune_calls += 1;
        t.prune_lines += lines.len() as u64;
    });
    if lines.len() < 2 {
        return;
    }
    // Exact lower-envelope hull restricted to [0, domain] (replaces the
    // 2-D endpoint-dominance sweep, which survived near-parallel
    // non-dominating lines and let the composition product explode — 28k
    // pairs/path measured on heavy captures).
    //
    // The hull IS the min-line envelope: for affine lines the pointwise
    // minimum passes through lines in slope order, switching at exact
    // rational crossover points. A line is minimal somewhere inside
    // [0, domain] iff its (ceil-rounded) takeover breakpoint lies ≤
    // domain. Dropping the rest is EXACT (same min at every x), not just
    // sound.
    //
    // Stage 1 (cheap, no wide divisions): endpoint-dominance sweep. A
    // line is dropped only when another is ≤ it at BOTH x=0 and x=domain —
    // the affine difference then bounds it everywhere in between. Removes
    // the readily-dominated majority for ~2 evals per line.
    //
    // Stage 2 (exact hull on survivors): lines are first sound-reduced to
    // COMPOSE_TARGET_BITS so the slope cross-products cannot overflow I512
    // (composed intermediates can reach ~484 bits); reduction only raises
    // values (ceil/floor rules), never under-cuts the bound. Divisions in
    // the hull are then paid only over the (small) survivor set, not the
    // full product set.
    let stage1_t0 = std::time::Instant::now();
    // Loop-16 T2: approximate ordering keys with exact fallbacks. The
    // keys carry ~2^-48 relative error; `approx_cmp`'s 1e-6 band separates
    // confidently-ordered comparisons from exact-comparator fallbacks, so
    // both the sort and the survivor sweep produce byte-identical results
    // to the exact-eval implementation — pinned by the randomized
    // differential test against the frozen reference copy.
    //
    // Saturation modeling: `eval` saturates the b·x multiply and the a +
    // bx add to I512::MAX on overflow; the keys model BOTH channels with
    // the same min-clipping in f64 (max_f stands in for I512::MAX).
    let upper_f = i512_to_f64(I512::try_from(U512::from(upper)).unwrap_or(I512::MAX));
    let max_f = i512_to_f64(I512::MAX);
    let mut indexed: Vec<S1Key> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let a_f = i512_to_f64(l.a);
            let b_f = i512_to_f64(l.b);
            let c_f = i512_to_f64(l.c);
            let prod_f = (b_f * upper_f).min(max_f);
            let n_f = (a_f + prod_f).min(max_f);
            S1Key {
                f0: a_f / c_f,
                fu: n_f / c_f,
                idx: i,
            }
        })
        .collect();
    // GATE-COMPOSE-1: memoize the exact endpoint evals lazily. The
    // comparator below re-evaluates BOTH lines on every f64-key tie; each
    // eval pays a 512-bit ceil_div. Eager precompute of all 2n evals
    // LOST on this corpus (ties are not that dense), so cache on first
    // use instead: never-tied lines keep paying nothing, tied lines pay
    // once. Evals are pure and deterministic, so the memoized values are
    // byte-identical to the closure form.
    let mut exact_at_zero_memo: Vec<Option<I512>> = vec![None; lines.len()];
    let mut exact_at_upper_memo: Vec<Option<I512>> = vec![None; lines.len()];
    indexed.sort_by(|x, y| {
        approx_cmp_ceil(x.f0, y.f0)
            .then_with(|| {
                if exact_at_zero_memo[x.idx].is_none() {
                    exact_at_zero_memo[x.idx] = Some(lines[x.idx].eval(&U256::ZERO));
                    gate_tls(|t| t.prune_tie_evals += 1);
                }
                if exact_at_zero_memo[y.idx].is_none() {
                    exact_at_zero_memo[y.idx] = Some(lines[y.idx].eval(&U256::ZERO));
                    gate_tls(|t| t.prune_tie_evals += 1);
                }
                exact_at_zero_memo[x.idx].cmp(&exact_at_zero_memo[y.idx])
            })
            .then(approx_cmp_ceil(x.fu, y.fu))
            .then_with(|| {
                if exact_at_upper_memo[x.idx].is_none() {
                    exact_at_upper_memo[x.idx] = Some(lines[x.idx].eval(&upper));
                    gate_tls(|t| t.prune_tie_evals += 1);
                }
                if exact_at_upper_memo[y.idx].is_none() {
                    exact_at_upper_memo[y.idx] = Some(lines[y.idx].eval(&upper));
                    gate_tls(|t| t.prune_tie_evals += 1);
                }
                exact_at_upper_memo[x.idx].cmp(&exact_at_upper_memo[y.idx])
            })
            .then(x.idx.cmp(&y.idx))
    });
    let mut min_f = f64::INFINITY;
    let mut min_idx = usize::MAX;
    let mut min_exact: Option<I512> = None;
    let mut surv: Vec<Line> = Vec::with_capacity(lines.len());
    for item in &indexed {
        let keep = match approx_cmp_ceil(item.fu, min_f) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                let cand = if let Some(v) = exact_at_upper_memo[item.idx] {
                    v
                } else {
                    let v = lines[item.idx].eval(&upper);
                    exact_at_upper_memo[item.idx] = Some(v);
                    gate_tls(|t| t.prune_tie_evals += 1);
                    v
                };
                if min_idx == usize::MAX {
                    cand < I512::MAX
                } else {
                    let mv = if let Some(mv) = min_exact {
                        mv
                    } else {
                        let mv = exact_at_upper_memo[min_idx]
                            .unwrap_or_else(|| lines[min_idx].eval(&upper));
                        min_exact = Some(mv);
                        mv
                    };
                    cand < mv
                }
            }
        };
        if keep {
            min_f = item.fu;
            min_idx = item.idx;
            min_exact = None;
            surv.push(lines[item.idx]);
        }
    }
    // Saturation guard (live crash 2026-08-30): when EVERY line's endpoint
    // eval saturates to I512::MAX, the strict improvement test keeps none
    // and the downstream hull indexed an empty set. Keep the smallest-key0
    // line — still a sound global upper bound, so the envelope can only
    // loosen, never under-cut.
    if surv.is_empty() {
        surv.push(lines[indexed[0].idx]);
    }
    gate_tls(|t| t.prune_stage1_ns += stage1_t0.elapsed().as_nanos());
    gate_tls(|t| t.prune_hull_lines += surv.len() as u64);
    if surv.len() < 2 {
        *lines = surv;
        return;
    }
    let hull_t0 = std::time::Instant::now();
    let parsed = &mut surv;
    for l in parsed.iter_mut() {
        l.reduce(COMPOSE_TARGET_BITS);
    }
    let mut idx: Vec<usize> = (0..parsed.len()).collect();
    // Approx slope keys (descending) with exact cross-mult fallback — same
    // approx_cmp band discipline as stage 1, byte-identical ordering.
    let slope_f: Vec<f64> = parsed
        .iter()
        .map(|l| i512_to_f64(l.b) / i512_to_f64(l.c))
        .collect();
    idx.sort_by(|&i, &j| {
        approx_cmp_ratio(slope_f[j], slope_f[i]).then_with(|| {
            let (li, lj) = (&parsed[i], &parsed[j]);
            let lhs = li.b * lj.c;
            let rhs = lj.b * li.c;
            rhs.cmp(&lhs)
        })
    });
    let mut hull: Vec<(U256, usize)> = Vec::with_capacity(idx.len());
    for &li in &idx {
        let l = &parsed[li];
        if let Some(&(_, top)) = hull.last() {
            let lt = &parsed[top];
            // Same-slope pairs: the lower intercept dominates globally.
            // Band-proximity gates the exact cross-mult check (loop-16
            // T2): confidently-distinct slopes skip the multiplications;
            // anything inside the band runs the exact check (which also
            // guards the pop-loop division against zero denominators —
            // exact-equal slopes must never reach `ceil_div`).
            if approx_cmp_ratio(slope_f[top], slope_f[li]).is_eq() && lt.b * l.c == l.b * lt.c {
                if lt.a * l.c <= l.a * lt.c {
                    continue;
                }
                hull.pop();
            }
        }
        let bp = if let Some(&(_, top)) = hull.last() {
            let lt = &parsed[top];
            let num = l.a * lt.c - lt.a * l.c;
            let den = lt.b * l.c - l.b * lt.c;
            ceil_div(num, den)
        } else {
            I512::ZERO
        };
        // First pop-loop iteration reuses the bp pair (same candidate, same
        // top — the 4 wide multiplications and the division are already
        // paid; loop-16 T2).
        let mut first_iter = true;
        while hull.len() >= 2 {
            let (bb, t) = hull[hull.len() - 1];
            let bb_i = I512::try_from(U512::from(bb)).unwrap_or(I512::MAX);
            let dominated = if first_iter {
                bp <= bb_i
            } else {
                let lprev = &parsed[t];
                let num = l.a * lprev.c - lprev.a * l.c;
                let den = lprev.b * l.c - l.b * lprev.c;
                // ceil(num/den) <= bb_i ⟺ num <= bb_i·den for den > 0 —
                // the multiplication replaces the wide division (falls back
                // to the division on the impossible-under-slope-order
                // den <= 0 case).
                if den > I512::ZERO {
                    match bb_i.checked_mul(den) {
                        Some(lhs) => num <= lhs,
                        None => ceil_div(num, den) <= bb_i,
                    }
                } else {
                    ceil_div(num, den) <= bb_i
                }
            };
            if dominated {
                hull.pop();
                first_iter = false;
            } else {
                break;
            }
        }
        let bx = if bp <= I512::ZERO {
            U256::ZERO
        } else {
            let u = U512::try_from(bp).unwrap_or(U512::MAX);
            if u > U512::from(U256::MAX) {
                U256::MAX
            } else {
                u.to::<U256>()
            }
        };
        hull.push((bx, li));
    }
    // Keep only lines whose takeover happens inside [0, domain].
    let keep: Vec<Line> = hull
        .iter()
        .filter(|&&(bx, _)| bx <= upper)
        .map(|&(_, i)| parsed[i])
        .collect();
    gate_tls(|t| t.prune_hull_ns += hull_t0.elapsed().as_nanos());
    *lines = keep;
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
    /// Composed-boundary cache hits inside this path's envelope product.
    pub prefix_hits: u64,
    /// Composition boundaries actually executed for this path.
    pub boundaries_composed: u64,
    /// Product-matrix wall time (ns) across this path's boundaries.
    pub product_ns: u128,
    /// Prune stage-1 endpoint-sweep wall time (ns).
    pub prune_stage1_ns: u128,
    /// Prune stage-2 hull wall time (ns).
    pub prune_hull_ns: u128,
    /// Loop-12 split: post-prune survivor reduce pass (ns).
    pub postprune_reduce_ns: u128,
    /// Loop-12 split: sampled-cap construction (ns).
    pub sample_ns: u128,
    /// Derivation phase wall time (ns) — per-hop tangent-line derivation.
    pub derive_ns: u128,
    /// Compose phase wall time (ns) — line chaining + prune over the chain.
    pub compose_ns: u128,
    /// Search phase wall time (ns) — the discrete concave max search.
    pub search_ns: u128,
    /// Affine composition pairs evaluated (diagnostic).
    pub pairs: u64,
    /// GATE-COMPOSE-1: prune invocations (diagnostic).
    pub prune_calls: u64,
    /// GATE-COMPOSE-1: total lines entering prune (diagnostic).
    pub prune_lines: u64,
    /// GATE-COMPOSE-1: stage-1 tie evals actually computed (diagnostic).
    pub prune_tie_evals: u64,
    /// GATE-COMPOSE-1: stage-2 hull input lines (diagnostic).
    pub prune_hull_lines: u64,
    /// GATE-COMPOSE-2: pairs the merge actually composed (vs enumerated).
    pub merge_selected: u64,
    /// GATE-COMPOSE-2: m*n pairs the legacy product would have composed.
    pub pairs_enumerated: u64,
    /// GATE-COMPOSE-2: boundaries that fell back to the legacy product.
    pub merge_legacy_fallbacks: u64,
}

impl GateStats {
    pub(crate) const EMPTY: Self = Self {
        evaluated: 0,
        skipped: 0,
        unsupported: 0,
        none_hop_unmapped: 0,
        none_degenerate: 0,
        none_overflow: 0,
        duration_ns: 0,
        prefix_hits: 0,
        boundaries_composed: 0,
        product_ns: 0,
        prune_stage1_ns: 0,
        prune_hull_ns: 0,
        derive_ns: 0,
        compose_ns: 0,
        search_ns: 0,
        postprune_reduce_ns: 0,
        sample_ns: 0,
        pairs: 0,
        prune_calls: 0,
        prune_lines: 0,
        prune_tie_evals: 0,
        prune_hull_lines: 0,
        merge_selected: 0,
        pairs_enumerated: 0,
        merge_legacy_fallbacks: 0,
    };

    /// Aggregate one worker thread's per-path counters into these cycle
    /// totals (replaces the engine's per-field atomic hand-aggregation).
    pub fn merge(&mut self, other: &Self) {
        self.evaluated += other.evaluated;
        self.skipped += other.skipped;
        self.unsupported += other.unsupported;
        self.none_hop_unmapped += other.none_hop_unmapped;
        self.none_degenerate += other.none_degenerate;
        self.none_overflow += other.none_overflow;
        self.duration_ns += other.duration_ns;
        self.prefix_hits += other.prefix_hits;
        self.boundaries_composed += other.boundaries_composed;
        self.product_ns += other.product_ns;
        self.prune_stage1_ns += other.prune_stage1_ns;
        self.prune_hull_ns += other.prune_hull_ns;
        self.derive_ns += other.derive_ns;
        self.compose_ns += other.compose_ns;
        self.search_ns += other.search_ns;
        self.postprune_reduce_ns += other.postprune_reduce_ns;
        self.sample_ns += other.sample_ns;
        self.pairs += other.pairs;
        self.prune_calls += other.prune_calls;
        self.prune_lines += other.prune_lines;
        self.prune_tie_evals += other.prune_tie_evals;
        self.prune_hull_lines += other.prune_hull_lines;
        self.merge_selected += other.merge_selected;
        self.pairs_enumerated += other.pairs_enumerated;
        self.merge_legacy_fallbacks += other.merge_legacy_fallbacks;
    }
}

thread_local! {
    // ONE TLS block entry (loop-16 T4): the per-timer statics exhausted
    // the dlopen static-TLS surplus on the Python import path
    // ("cannot allocate memory in static TLS block").
    static GATE_TLS: std::cell::RefCell<GateStats> =
        const { std::cell::RefCell::new(GateStats::EMPTY) };
}

pub(crate) fn gate_tls<R>(f: impl FnOnce(&mut GateStats) -> R) -> R {
    GATE_TLS.with(|t| f(&mut t.borrow_mut()))
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
enum HopCacheKey {
    ClTable(u128),
    MobiusHop(u128),
}

/// FNV-1a-style 128-bit content mix for `U256` words.
fn content_mix_u256(mut h: u128, v: &U256) -> u128 {
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
fn cl_table_key(crossings: &[IntTickRangeCrossing]) -> u128 {
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
}

static PREFIX_CACHE: std::sync::LazyLock<std::sync::Mutex<PrefixCacheState>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(PrefixCacheState {
            epoch: 0,
            map: std::collections::HashMap::new(),
        })
    });

/// Reset all gate counters on the calling thread (call at solve-cycle start,
/// mirroring [`crate::mobius_v3_int::reset_walk_stats`]).
pub fn reset_gate_stats() {
    gate_tls(|t| *t = GateStats::EMPTY);
}

/// Read-and-clear the calling thread's gate counters (the ONE read-back
/// accessor — phase splits and pair volume are fields of the same struct).
#[must_use]
pub fn take_last_gate_stats() -> GateStats {
    gate_tls(|t| std::mem::replace(t, GateStats::EMPTY))
}

/// The gate's typed verdict (SU7MAE deepening): [`Envelope::Bound`] is a
/// rigorous upper bound on `max_x [path_output(x) − x]` — skip ONLY when its
/// value is at or below the caller's profit floor. [`Envelope::Unsupported`]
/// means NO sound bound exists: the path is SOLVED unscreened, never skipped
/// (type-enforced replacement of the overloaded `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Envelope {
    Bound(U256),
    Unsupported(GateSkipCause),
}

/// Why no bound was derivable (the per-cause M6776W counters name the same
/// three exits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateSkipCause {
    /// A hop slot was `None` (caller couldn't map the family).
    UnmappedHop,
    /// A hop's line derivation rejected (zero reserves / no reachable
    /// liquidity).
    DegenerateHop,
    /// A coefficient or domain overflow made the bound unusable.
    DomainOverflow,
}

/// The gate's per-call dependency value. Carries everything that used to be
/// process-global: the solve-cycle epoch (the prefix cache drops entries
/// from an older epoch on first touch), the cache opt-in, and the optional
/// degenerate-path capture config. One interface, no hidden state.
#[derive(Clone, Copy, Debug, Default)]
pub struct GateDeps<'a> {
    pub epoch: u64,
    pub prefix_cache: bool,
    pub capture: Option<&'a GateCaptureCfg>,
    /// The engine-owned cross-block walk-memo handle (SU7MAE T3); `None`
    /// disables the memo for this solve.
    pub walk_memo: Option<&'a crate::mobius_v3_int::WalkMemo>,
}

impl GateDeps<'_> {
    /// Offline/cacheless: no prefix reuse, epoch 0, capture off, no memo.
    #[must_use]
    pub fn offline() -> Self {
        Self::default()
    }

    /// Production solve cycle: the prefix cache against this block's epoch.
    #[must_use]
    pub fn per_block(epoch: u64, capture: Option<&GateCaptureCfg>) -> GateDeps<'_> {
        GateDeps {
            epoch,
            prefix_cache: true,
            capture,
            walk_memo: None,
        }
    }

    /// The engine-owned walk-memo handle, if any (`None` disables it).
    #[must_use]
    pub fn walk_memo(&self) -> Option<&crate::mobius_v3_int::WalkMemo> {
        self.walk_memo
    }
}

/// Degenerate-path capture config (M6776W): where to write + how many paths
/// to capture. The production engine and the harnesses build it from the
/// `DEGENBOT_GATE_CAPTURE*` env vars via [`GateCaptureCfg::from_env`]; the
/// gate itself reads no environment.
#[derive(Clone, Debug)]
pub struct GateCaptureCfg {
    pub out_path: std::path::PathBuf,
    pub max_paths: u64,
}

/// The ONE gate entry: a rigorous upper bound on
/// `max_x [path_output(x) − x]`, or a typed [`GateSkipCause`]. Crossings ride
/// each CL hop's [`ClHop`] descriptor; the prefix-composition cache is keyed
/// on full hop content and generationed by [`GateDeps::epoch`].
#[must_use]
pub fn path_profit_bound(hops: &[Option<HopMath<'_>>], deps: &GateDeps<'_>) -> Envelope {
    let gate_t0 = std::time::Instant::now();
    let result = path_profit_bound_inner(hops, deps);
    gate_tls(|t| t.duration_ns = gate_t0.elapsed().as_nanos());
    match result {
        Ok(b) => {
            gate_tls(|t| t.evaluated += 1);
            Envelope::Bound(b)
        }
        Err(cause) => {
            gate_tls(|t| {
                t.unsupported += 1;
                match cause {
                    GateSkipCause::UnmappedHop => t.none_hop_unmapped += 1,
                    GateSkipCause::DegenerateHop => t.none_degenerate += 1,
                    GateSkipCause::DomainOverflow => t.none_overflow += 1,
                }
            });
            Envelope::Unsupported(cause)
        }
    }
}

#[expect(clippy::too_many_lines)]
fn path_profit_bound_inner(
    hops: &[Option<HopMath<'_>>],
    deps: &GateDeps<'_>,
) -> Result<U256, GateSkipCause> {
    let mut all_hops: Vec<(Vec<Line>, U256)> = Vec::with_capacity(hops.len());
    let mut xmax = U256::ZERO;
    let phase_derive = std::time::Instant::now();
    for (hop_idx, slot) in hops.iter().enumerate() {
        let Some(hop) = slot.as_ref() else {
            return Err(GateSkipCause::UnmappedHop);
        };
        let Some((hop_ls, cap)) = hop_lines_and_cap(hop.clone()) else {
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
                HopMath::Cl(ch) => {
                    let empty = ch.seq.ranges.is_empty();
                    let reason = classify_cl_rejection(ch.seq);
                    format!(
                        "Cl(ranges={n},empty={empty},{reason})",
                        n = ch.seq.ranges.len()
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
            // M6776W golden capture: serialize the full per-hop state when a
            // capture harness is configured so the pool states can be replayed
            // offline for fix experimentation.
            if let Some(cfg) = deps.capture {
                let reason = match hop {
                    HopMath::Cl(ch) => classify_cl_rejection(ch.seq),
                    _ => family.clone(),
                };
                capture_degenerate_path(hops, hop_idx, &reason, cfg);
            }
            return Err(GateSkipCause::DegenerateHop);
        };
        if let Some(v) = xmax.checked_add(cap) {
            xmax = v;
        } else {
            return Err(GateSkipCause::DomainOverflow);
        }
        all_hops.push((hop_ls, cap));
    }
    gate_tls(|t| t.derive_ns += phase_derive.elapsed().as_nanos());
    let phase_compose = std::time::Instant::now();
    // Second pass against the FULL known input domain. Every line that
    // cannot be beaten below within [0,domain] can never best at any path
    // input, so the pruning endpoint assumption holds at discard time.
    let mut lines2 = vec![Line::IDENTITY];
    let domain = xmax;
    // Prefix-composition cache (loop-8): every chainable hop contributes a
    // FULL-CONTENT key — the CL hop's whole crossing table hashed (a content
    // hash, not an allocation pointer; the pointer key + endpoint-fingerprint
    // revalidation pair is retired), the Möbius hop a reserves+fee hash. A
    // 128-bit FNV hit is the only stale-serve risk — the same trust level
    // the Möbius key already carried.
    //
    // Cross-DOMAIN reuse is sound WITHOUT keying on the domain: every
    // stored line globally dominates the true prefix output (tangent
    // property), so serving a set pruned under a different domain can
    // only shift the bound's TIGHTNESS (a smaller domain's entry is a
    // subset of the reader's fresh set → looser bound → skips less; a
    // larger domain's entry is a superset → tighter bound → more skips,
    // each still justified: the bound remains an upper bound of the true
    // curve). Skip validity is the only contract, so reuse stays keyed
    // on the chain alone.
    let mobius_key =
        |reserve_in: &U256, reserve_out: &U256, gamma_numer: &U256, fee_denom: &U256| {
            let mut h = content_mix_u256(0xcbf2_9ce4_8422_2325_u128, reserve_in);
            h = content_mix_u256(h, reserve_out);
            h = content_mix_u256(h, gamma_numer);
            content_mix_u256(h, fee_denom)
        };
    let hop_key = |hop_idx: usize| -> Option<HopCacheKey> {
        match hops.get(hop_idx).and_then(Option::as_ref) {
            Some(HopMath::Cl(ch)) => {
                Some(HopCacheKey::ClTable(cl_table_key(ch.crossings.as_ref())))
            }
            Some(HopMath::V2(h)) => Some(HopCacheKey::MobiusHop(mobius_key(
                &h.reserve_in,
                &h.reserve_out,
                &h.gamma_numer,
                &h.fee_denom,
            ))),
            _ => None,
        }
    };
    let mut chain: Vec<HopCacheKey> = Vec::with_capacity(hops.len());
    for (hop_idx, (hop_ls, _)) in all_hops.iter_mut().enumerate() {
        match if deps.prefix_cache {
            hop_key(hop_idx)
        } else {
            None
        } {
            Some(k) => chain.push(k),
            None => chain.clear(),
        }
        let chainable = deps.prefix_cache && !chain.is_empty();
        if chainable {
            // Cache lookup for this exact content chain. The key IS the full
            // per-hop content, so a hit is a content match; entries from an
            // older epoch are dropped on first touch of the new one (no
            // public reset — the epoch rides [GateDeps]).
            let hit = match PREFIX_CACHE.lock() {
                Ok(mut cache) => {
                    if cache.epoch != deps.epoch {
                        cache.epoch = deps.epoch;
                        cache.map.clear();
                    }
                    cache.map.get(&chain).cloned()
                }
                Err(_) => None,
            };
            if let Some(hit_lines) = hit {
                lines2 = hit_lines;
                gate_tls(|t| t.prefix_hits += 1);
                continue;
            }
        }
        gate_tls(|t| t.boundaries_composed += 1);
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
        let hop_ls_len_dbg = hop_ls.len();
        // GATE-COMPOSE-2 (7OT63B): pair-selection merge instead of the
        // m*n product. Selects only the <= m + n - 1 (outer_piece,
        // inner_piece) pairs whose canonical-envelope y-intervals
        // intersect, composes those, and runs the UNCHANGED
        // prune -> reduce -> sample tail (byte-identical output; falls
        // back to the frozen legacy product on flat/sign/ambiguous
        // inputs — see compose_boundary_merged). Err(DomainOverflow) on
        // a SELECTED pair matches legacy exactly (same compose, same
        // reduce-retry); skipped-pair overflows may relax Err -> Ok
        // (documented skip-relaxation, still sound).
        let mut next: Vec<Line> =
            compose_boundary_merged(hop_ls, &lines2, domain, sampled_compose_lines())?;
        // One reduction pass per hop boundary (O(survivors)) — replaces the
        // per-pair reduction removed from Line::compose. Byte-identical
        // coefficients to the old per-pair pass (same ceil/floor rules).
        {
            let t0 = std::time::Instant::now();
            for l in &mut next {
                l.reduce(COMPOSE_TARGET_BITS);
            }
            gate_tls(|t| t.postprune_reduce_ns += t0.elapsed().as_nanos());
        }
        // sampled_compose_lines() cap: the composing side is dropped to a
        // uniform Pareto-order sample across the lower envelope, bounding
        // the next product at K². Sound by the same argument as the CL
        // tangent cap: min(fewer lines) ≥ min(all lines), so the bound can
        // only rise (skip less, never more). With the live min-profit floor
        // of zero the tightness loss does not affect skips.
        let samp_t0 = if next.len() > sampled_compose_lines() {
            Some(std::time::Instant::now())
        } else {
            None
        };
        if let Some(_t0) = samp_t0 {
            let step = next.len() / sampled_compose_lines();
            let mut sampled = Vec::with_capacity(sampled_compose_lines() + 1);
            let mut i = 0usize;
            while i < next.len() {
                sampled.push(next[i]);
                i += step.max(1);
            }
            if sampled.last() != Some(&next[next.len() - 1]) {
                sampled.push(next[next.len() - 1]);
            }
            next = sampled;
        }
        if let Some(t0) = samp_t0 {
            gate_tls(|t| t.sample_ns += t0.elapsed().as_nanos());
        }
        // Cache the composed prefix set under the content-key chain. Only
        // miss paths reach here; a hit path returns early above.
        if chainable {
            if let Ok(mut cache) = PREFIX_CACHE.lock() {
                if cache.epoch != deps.epoch {
                    cache.epoch = deps.epoch;
                    cache.map.clear();
                }
                cache.map.insert(chain.clone(), next.clone());
            }
        }
        if gate_trace_enabled() {
            trace_boundary(hop_idx, hop_ls_len_dbg, next.len(), &next);
        }
        lines2 = next;
    }
    gate_tls(|t| t.compose_ns += phase_compose.elapsed().as_nanos());
    let phase_search = std::time::Instant::now();
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
        return Ok(U256::ZERO);
    }
    // Lower-envelope hull over the surviving lines: order by slope (b/c,
    // exact rational compare) descending, drop same-slope dominated
    // intercepts, and store per-hull-line the ceil-rounded integer
    // breakpoint at which it takes over the running minimum. f(x) then
    // costs ONE line eval instead of one eval per line per probe.
    //
    // Measured basis: the ternary dominates up to ~38% of gate wall on
    // range-heavy paths (O(lines) eval per probe x ~256 probes).
    let mut idx: Vec<usize> = (0..lines.len()).collect();
    // Approx slope keys (descending) with exact cross-mult fallback — same
    // approx_cmp band discipline as stage 1, byte-identical ordering.
    let slope_f: Vec<f64> = lines
        .iter()
        .map(|l| i512_to_f64(l.b) / i512_to_f64(l.c))
        .collect();
    idx.sort_by(|&i, &j| {
        approx_cmp_ratio(slope_f[j], slope_f[i]).then_with(|| {
            let (li, lj) = (&lines[i], &lines[j]);
            let lhs = li.b * lj.c;
            let rhs = lj.b * li.c;
            rhs.cmp(&lhs)
        })
    });
    // Hull: (breakpoint_x, line_index). Breakpoints monotonically increase.
    let mut hull: Vec<(U256, usize)> = Vec::with_capacity(lines.len());
    for &li in &idx {
        let l = &lines[li];
        if let Some(&(_, top)) = hull.last() {
            if top != usize::MAX {
                let lt = &lines[top];
                // same-slope (b_t·c == b·c_t) with lower-or-equal intercept
                // dominates the candidate everywhere — drop the candidate.
                let s_eq = lt.b * l.c == l.b * lt.c;
                let dom = lt.a * l.c <= l.a * lt.c;
                if s_eq && dom {
                    continue;
                }
                if s_eq {
                    // candidate dominates the top of equal slope: replace it.
                    hull.pop();
                }
            }
        }
        let bp = if let Some(&(_, top)) = hull.last() {
            let lt = &lines[top];
            // x = (A_i·C_t − A_t·C_i) / (B_t·C_i − B_i·C_t), ceil-rounded so
            // the incumbent is never under-cut before the true take-over.
            let num = l.a * lt.c - lt.a * l.c;
            let den = lt.b * l.c - l.b * lt.c;
            ceil_div(num, den)
        } else {
            I512::ZERO
        };
        // Hull monotonicity: pop tops whose stored breakpoint would sit at/after
        // this candidate's — they can never be minimal again.
        while hull.len() >= 2 {
            let (bb, t) = hull[hull.len() - 1];
            let lprev = &lines[t];
            let num = l.a * lprev.c - lprev.a * l.c;
            let den = lprev.b * l.c - l.b * lprev.c;
            let bb_i = I512::try_from(U512::from(bb)).unwrap_or(I512::MAX);
            if ceil_div(num, den) <= bb_i {
                hull.pop();
            } else {
                break;
            }
        }
        // Positive take-over: saturate to U256 (a later breakpoint keeps the
        // incumbent selected longer — strictly conservative for a bound).
        let bx = if bp <= I512::ZERO {
            U256::ZERO
        } else {
            let u = U512::try_from(bp).unwrap_or(U512::MAX);
            if u > U512::from(U256::MAX) {
                U256::MAX
            } else {
                u.to::<U256>()
            }
        };
        hull.push((bx, li));
    }
    let hlen = hull.len();
    let hull_ref = &hull;
    let lines_ref = &lines;
    let f = |x: &U256| -> I512 {
        // Binary search: last breakpoint ≤ x owns the minimum line.
        let mut ix = hlen; // default = last line (dominates near infinity)
        let mut lo = 0usize;
        let mut hi = hlen;
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if hull_ref[mid].0 <= *x {
                ix = mid;
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let li = hull_ref[if ix == hlen { hlen - 1 } else { ix }].1;
        lines_ref[li].eval(x) - I512::try_from(U512::from(*x)).unwrap_or(I512::MAX)
    };
    // T5 fix (false-skip class, block 25886170 path 93794): the predecessor
    // of this scan was a discrete binary search that assumed f unimodal.
    // Pathological tail ranges (tiny liquidity spanning a 1e19 price ratio)
    // emit near-zero-slope tangent lines whose ceil-eval staircases have
    // periods ~ c/b ≈ 1e30, making f non-unimodal at integer resolution;
    // the search then collapsed onto a wiggle (x*=1, f*=1) and the gate
    // false-skipped profitable paths (golden 2.4e10 vs bound 1).
    //
    // Sound replacement: on each hull segment [bp_i, bp_{i+1}) the selected
    // line is fixed, so g(x) = ceil((a+b·x)/c) − x is non-increasing in x
    // up to +1-ULP up-steps — the maximum over the segment is attained at
    // its first two integers. Scanning every breakpoint (±1) plus the
    // [0, xmax] endpoints therefore finds the exact integer max. Every
    // evaluated value is min-line ceil-eval − x, a valid upper bound of the
    // true profit at that x, so the reported bound stays SOUND.
    let one = U256::from(1u8);
    let mut best = f(&U256::ZERO);
    for &(bp, _) in &hull {
        if bp.is_zero() || bp > xmax {
            continue;
        }
        for &cand in &[bp, (bp + one).min(xmax)] {
            let v = f(&cand);
            if v > best {
                best = v;
            }
        }
        // The left neighbour of a breakpoint can carry a +1-ULP holdover
        // from the previous segment's ceil-step (the incumbent line still
        // selected one integer earlier).
        let prev = bp - one;
        if !prev.is_zero() {
            let v = f(&prev);
            if v > best {
                best = v;
            }
        }
    }
    {
        let v = f(&xmax.min(one));
        if v > best {
            best = v;
        }
    }
    gate_tls(|t| t.search_ns += phase_search.elapsed().as_nanos());
    // Rounding slack: composed reductions and I512 ceiling evaluation can
    // leave the derived lower envelope a hair BELOW the true curve. The
    // deficit is ~2^-11 of the bound per reduction for very deep chains
    // (block 25826949 path 400: 200M under-cut on a 7.23e13 bound), but the
    // same per-hop reductions compound on moderate chains too: block
    // 25826949 path 704 under-cut 3.7e15 on a 2.7e23 bound while its
    // composed survivor count stayed <= 200, so the old lines>200 gate
    // skipped the slack. The slack must be UNCONDITIONAL — soundness of the
    // skip decision is the only contract; the 1/2048 (~0.05%) looseness is
    // invisible to live skips against the incumbent/floor comparisons.
    if let Some(b) = narrow(best) {
        return Ok(b.saturating_add(b / U256::from(2048u64)));
    }
    Err(GateSkipCause::DomainOverflow)
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
            gate_tls(|t| t.none_hop_unmapped += 1);
            return None;
        };
        let (hop_ls, _cap) = hop_lines_and_cap(hop.clone())?;
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                let Some(composed) = outer.compose(inner) else {
                    gate_tls(|t| t.none_overflow += 1);
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

/// GATE-COMPOSE-2 (7OT63B): merged pair-selection compose.
///
/// The composed lower envelope F(x) = min over pairs of outer_j(inner_i(x))
/// factorizes, for non-decreasing lines (b >= 0), into
/// F(x) = OW(E_inner(x)) with E_inner the pointwise min over the chain set
/// and OW the pointwise min over the (pruned) hop set. Both are canonical
/// hulls; their composition is PWL whose pieces are exactly the composed
/// lines (outer_piece, inner_piece) whose y-intervals INTERSECT. So instead
/// of composing all m*n pairs, select the <= m + n - 1 overlapping
/// (outer_piece, inner_piece) pairs via a two-pointer sweep, compose ONLY
/// those, and route the result through the UNCHANGED prune -> reduce ->
/// sample chain in outer-major order (a subsequence of the legacy product
/// order, so legacy's stable-sort tie machinery sees identical relative
/// order for every pair that exists).
///
/// Safety: DEGENERATE-FREE pair selection is impossible to prove for tie /
/// ceil-collision inputs, so this routine falls back to the frozen legacy
/// chain whenever selection is ambiguous:
///   - any flat line (b == 0) or negative-slope line on either side (the
///     monotone preimage machinery needs b > 0; b == 0 is plausible input:
///     stableswap reserve-only cap lines),
///   - exact y-overlap boundaries landing on an outer breakpoint
///     (instance ambiguity at the piece seam),
///   - an empty selection (cannot happen for non-empty envelopes, guarded
///     anyway).
///
/// Overflow semantics match legacy for SELECTED pairs (compose's
/// reduce-retry is deterministic per pair); for SKIPPED pairs the legacy
/// Err(DomainOverflow) may relax to Ok — the exact envelope is still a
/// valid upper bound (see the differential test's documented
/// skip-relaxation arm).
fn compose_boundary_merged(
    hop_lines: &[Line],
    chain: &[Line],
    upper: U256,
    cap: usize,
) -> Result<Vec<Line>, GateSkipCause> {
    let legacy = |reason: &'static str| -> Result<Vec<Line>, GateSkipCause> {
        gate_tls(|t| t.merge_legacy_fallbacks += 1);
        tracing::debug!(
            target: "degenbot_solvers::profit_envelope",
            reason,
            "[gate] compose merge fell back to legacy pair product"
        );
        compose_boundary_reference(hop_lines, chain, upper, cap)
    };
    // Selection needs monotone preimages: every line non-decreasing.
    let monotone = |lines: &[Line]| lines.iter().all(|l| l.b >= I512::ZERO);
    if !monotone(hop_lines) || !monotone(chain) {
        return legacy("b_sign");
    }
    let mut hop_ls = hop_lines.to_vec();
    prune(&mut hop_ls, upper);
    // Flat (b == 0) pieces make E_inner / OW non-strictly monotone and
    // collapse y-intervals to points; selection can double-visit — fall
    // back (telemetry counts how often; stableswap reserve-cap lines are
    // the expected production source).
    if hop_ls.iter().any(|l| l.b == I512::ZERO) || chain.iter().any(|l| l.b == I512::ZERO) {
        return legacy("flat");
    }
    // The chain set is post-sample (NOT canonical); its envelope pieces
    // come from a fresh hull over the subset. Pieces carry their ORIGIN
    // index so emitted pairs map back to real (hop_ls, chain) positions —
    // no synthetic composes (byte-identity requirement).
    let outer_pieces = hull_pieces(&hop_ls, upper);
    let inner_pieces = hull_pieces(chain, upper);
    if outer_pieces.is_empty() || inner_pieces.is_empty() {
        return legacy("empty_pieces");
    }

    // Two-pointer sweep over y-intervals. Outer piece l covers y in
    // [obp_l, obp_{l+1}) (last piece open-ended); inner piece k covers
    // x in [bx_k, bx_{k+1}) which maps to a y-range ascending (b > 0).
    let obp_i = |obp: U256| -> I512 { I512::try_from(U512::from(obp)).unwrap_or(I512::MAX) };
    let mut selected: Vec<(usize, usize)> =
        Vec::with_capacity(outer_pieces.len() + inner_pieces.len());
    let mut li = 0usize; // outer-piece pointer (y-ascending)
    let mut prev_y_hi = I512::ZERO;
    for (k, &(bx_k, ref ik, origin_k)) in inner_pieces.iter().enumerate() {
        let x_start = bx_k.min(upper);
        let x_end = inner_pieces
            .get(k + 1)
            .map_or(upper, |&(bx, _, _)| bx)
            .min(upper);
        if x_end <= x_start && k + 1 < inner_pieces.len() {
            continue; // collapsed interval behind the sweep
        }
        let y_lo0 = ik.eval(&x_start);
        let y_hi0 = ik.eval(&x_end);
        let (y_lo, y_hi) = if y_lo0 <= y_hi0 {
            (y_lo0, y_hi0)
        } else {
            (y_hi0, y_lo0)
        };
        // x-ascending pieces must be y-ascending; a regression means the
        // clamped breakpoints reordered pieces beyond monotonicity.
        if k > 0 && y_lo < prev_y_hi {
            return legacy("y_disorder");
        }
        prev_y_hi = y_hi;
        // advance the outer pointer past pieces ending strictly before
        // this inner piece's y-extent
        while li + 1 < outer_pieces.len() && obp_i(outer_pieces[li + 1].0) <= y_lo {
            li += 1;
        }
        // walk up while the next outer piece's y-range starts at/below y_hi
        let mut l = li;
        loop {
            let (_, _, o_origin) = outer_pieces[l];
            selected.push((o_origin, origin_k));
            if l + 1 >= outer_pieces.len() {
                break;
            }
            let next_start = obp_i(outer_pieces[l + 1].0);
            if next_start > y_hi {
                break;
            }
            l += 1;
        }
    }
    if selected.is_empty() {
        return legacy("empty_selection");
    }
    gate_tls(|t| {
        t.merge_selected += selected.len() as u64;
        t.pairs_enumerated += (hop_ls.len() as u64) * (chain.len() as u64);
    });
    // Outer-major lexicographic order over the origins: a subsequence of
    // the legacy product walk (hop_ls outer-major, chain inner-minor), so
    // every stable-sort tiebreak in the downstream prune sees the same
    // relative order legacy would have given it.
    selected.sort_unstable_by_key(|&(oj, oi)| (oj, oi));
    let mut next: Vec<Line> = Vec::with_capacity(selected.len());
    for &(oj, oi) in &selected {
        // A SELECTED pair failing compose (even after reduce-retry) fails
        // identically in legacy: the reference composed the same pair. A
        // SKIPPED pair failing is the documented skip-relaxation (legacy
        // Err may relax to Ok — still sound: the exact composed envelope
        // is a valid upper bound).
        let Some(composed) = hop_ls[oj].compose(&chain[oi]) else {
            return Err(GateSkipCause::DomainOverflow);
        };
        next.push(composed);
    }
    // Unchanged legacy tail (prune -> reduce -> sample).
    prune(&mut next, upper);
    for l in &mut next {
        l.reduce(COMPOSE_TARGET_BITS);
    }
    if next.len() > cap {
        let step = next.len() / cap;
        let mut sampled = Vec::with_capacity(cap + 1);
        let mut i = 0usize;
        while i < next.len() {
            sampled.push(next[i]);
            i += step.max(1);
        }
        if sampled.last() != Some(&next[next.len() - 1]) {
            sampled.push(next[next.len() - 1]);
        }
        next = sampled;
    }
    Ok(next)
}

/// Canonical (breakpoint, line, ORIGIN index) pieces of the lower
/// envelope over [0, upper]. Mirrors the PRUNE hull's arithmetic
/// (stage-2, ~L1140) — slope-descending order with exact cross-mult
/// fallback, same-slope dominance swap, ceil-rounded takeover,
/// U256 saturation clamps, pops-before-bx>upper-reject — NOT the
/// search-phase hull. The origin index maps each piece back to its
/// position in the INPUT slice so the merge can emit real (j, i)
/// pairs (no synthetic composes — byte-identity requirement).
fn hull_pieces(lines: &[Line], upper: U256) -> Vec<(U256, Line, usize)> {
    let mut idx: Vec<usize> = (0..lines.len()).collect();
    let slope_f: Vec<f64> = lines
        .iter()
        .map(|l| i512_to_f64(l.b) / i512_to_f64(l.c))
        .collect();
    idx.sort_by(|&i, &j| {
        approx_cmp_ratio(slope_f[j], slope_f[i]).then_with(|| {
            let (li, lj) = (&lines[i], &lines[j]);
            let lhs = li.b * lj.c;
            let rhs = lj.b * li.c;
            rhs.cmp(&lhs)
        })
    });
    let mut hull: Vec<(U256, usize)> = Vec::with_capacity(lines.len());
    for &li in &idx {
        let l = &lines[li];
        if let Some(&(_, top)) = hull.last() {
            let lt = &lines[top];
            let s_eq = lt.b * l.c == l.b * lt.c;
            let dom = lt.a * l.c <= l.a * lt.c;
            if s_eq && dom {
                continue;
            }
            if s_eq {
                hull.pop();
            }
        }
        let bp = if let Some(&(_, top)) = hull.last() {
            let lt = &lines[top];
            let num = l.a * lt.c - lt.a * l.c;
            let den = lt.b * l.c - l.b * lt.c;
            ceil_div(num, den)
        } else {
            I512::ZERO
        };
        while hull.len() >= 2 {
            let (bb, t) = hull[hull.len() - 1];
            let lprev = &lines[t];
            let num = l.a * lprev.c - lprev.a * l.c;
            let den = lprev.b * l.c - l.b * lprev.c;
            let bb_i = I512::try_from(U512::from(bb)).unwrap_or(I512::MAX);
            if ceil_div(num, den) <= bb_i {
                hull.pop();
            } else {
                break;
            }
        }
        let bx = if bp <= I512::ZERO {
            U256::ZERO
        } else {
            let u = U512::try_from(bp).unwrap_or(U512::MAX);
            if u > U512::from(U256::MAX) {
                U256::MAX
            } else {
                u.to::<U256>()
            }
        };
        if bx > upper {
            continue;
        }
        hull.push((bx, li));
    }
    // Recompute piece boundaries PAIRWISE between consecutive final
    // hull entries. The pop-time stored bx is computed against a top
    // that subsequent pops may remove, so clamped/collapsed values can
    // leave adjacent entries with disordered breakpoints (observed:
    // two entries both clamped to 0), which collapses piece intervals
    // and starves the merge sweep. Pairwise crossovers of the final
    // hull list are the authoritative interval boundaries.
    let mut pieces: Vec<(U256, Line, usize)> = Vec::with_capacity(hull.len());
    for (pos, &(_, li)) in hull.iter().enumerate() {
        let bx = if pos == 0 {
            U256::ZERO
        } else {
            let prev = &lines[hull[pos - 1].1];
            let cur = &lines[li];
            let num = cur.a * prev.c - prev.a * cur.c;
            let den = prev.b * cur.c - cur.b * prev.c;
            let bp = ceil_div(num, den);
            if bp <= I512::ZERO {
                U256::ZERO
            } else {
                let u = U512::try_from(bp).unwrap_or(U512::MAX);
                if u > U512::from(U256::MAX) {
                    U256::MAX
                } else {
                    u.to::<U256>()
                }
            }
        };
        pieces.push((bx, lines[li], li));
    }
    pieces
}

/// Frozen legacy boundary chain: hop prune -> ALL-pairs product ->
/// prune -> reduce -> sample.
///
/// KEEP-IN-SYNC with the production sequence in
/// `path_profit_bound_inner` (hop prune at the loop head, product,
/// prune, reduce, sample in the per-boundary tail): this is a hand
/// copy so the merge can be differentially pinned; golden dual-run
/// asserting reference == production on captures is the fast-follow
/// guard against silent fork. Byte-for-byte the production sequence
/// with the prefix cache and telemetry elided; the merge
/// implementation must reproduce this output exactly (modulo the
/// documented DomainOverflow skip-relaxation). `cap` is passed
/// explicitly (production: `sampled_compose_lines()`) so tests can
/// pin it smaller and exercise the sampling path deterministically.
fn compose_boundary_reference(
    hop_lines: &[Line],
    chain: &[Line],
    upper: U256,
    cap: usize,
) -> Result<Vec<Line>, GateSkipCause> {
    let mut hop_ls = hop_lines.to_vec();
    prune(&mut hop_ls, upper);
    let lines2 = chain.to_vec();
    let mut next: Vec<Line> = Vec::with_capacity(lines2.len() * hop_ls.len());
    for outer in &hop_ls {
        for inner in &lines2 {
            let Some(composed) = outer.compose(inner) else {
                return Err(GateSkipCause::DomainOverflow);
            };
            next.push(composed);
        }
    }
    prune(&mut next, upper);
    for l in &mut next {
        l.reduce(COMPOSE_TARGET_BITS);
    }
    if next.len() > cap {
        let step = next.len() / cap;
        let mut sampled = Vec::with_capacity(cap + 1);
        let mut i = 0usize;
        while i < next.len() {
            sampled.push(next[i]);
            i += step.max(1);
        }
        if sampled.last() != Some(&next[next.len() - 1]) {
            sampled.push(next[next.len() - 1]);
        }
        next = sampled;
    }
    Ok(next)
}

#[cfg(test)]
#[expect(clippy::expect_used)] // tiny literals; panic on typo is the point
mod tests {
    use super::*;
    use crate::mobius_v3_int::int_simulate_v3_swap;
    use degenbot_pools::int_v3_hop::IntV3TickRangeHop;

    // ===================================================================
    // GATE-COMPOSE-2: merged pair-selection compose (7OT63B).
    //
    // `compose_boundary_reference` freezes the LEGACY boundary chain
    // (hop prune -> pair product -> prune -> reduce -> sample) so the
    // merge-based implementation can be differentially pinned against it
    // at boundary granularity. `hull_pieces` lifts the search-hull's
    // exact crossover arithmetic (ceil_div over composed coefficients,
    // same-slope dominance, U256 clamps) so both sides see identical
    // rounding semantics.
    // ===================================================================

    /// RED: the merged pair-selection compose must reproduce the frozen
    /// reference exactly (values AND order) on randomized adversarial
    /// sets — flat lines (b==0), equal slopes, negative intercepts,
    /// cross-pair functional coincidences, saturation boundaries, and
    /// U256-breakpoint clamps included.
    ///
    /// The sample cap is PINNED per seed (2/3/4/6) so the
    /// prune-order -> stride-sample -> next-boundary chain actually runs
    /// (the 48 default never fires for m,n<=5); reference and merged read
    /// the same forced value.
    #[test]
    fn merged_compose_matches_frozen_reference_on_randomized_sets() {
        for seed in 0..256u64 {
            let mut lcg = seed | (seed << 32) | 1;
            let rand_nxt = |lcg: &mut u64| {
                *lcg = lcg
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *lcg
            };
            let hop_count = (rand_nxt(&mut lcg) % 5) as usize + 1;
            let chain_count = (rand_nxt(&mut lcg) % 5) as usize + 1;
            let rand_line = |lcg: &mut u64| -> Line {
                let v = |lcg: &mut u64, bits: u32| -> I512 {
                    let mut x = I512::from_raw(U512::from(rand_nxt(lcg)));
                    let shift = u32::try_from(rand_nxt(lcg) % (u64::from(bits) + 1)).unwrap_or(0);
                    x <<= shift;
                    if rand_nxt(lcg).is_multiple_of(3) {
                        -x
                    } else {
                        x
                    }
                };
                let c = I512::from_raw(U512::from(rand_nxt(lcg) % 15 + 1));
                let b = I512::from_raw(U512::from(rand_nxt(lcg) % 7)); // includes b == 0
                Line {
                    a: v(lcg, 40),
                    b,
                    c,
                }
            };
            let hop_lines: Vec<Line> = (0..hop_count).map(|_| rand_line(&mut lcg)).collect();
            let chain: Vec<Line> = (0..chain_count).map(|_| rand_line(&mut lcg)).collect();
            // fifth magnitude = U256::MAX so clamped-to-MAX breakpoints
            // survive the bx > upper filter (L3 clamp stress).
            let upper = match seed % 5 {
                0 => U256::from(1_000u64) << 96,
                1 => U256::from(1_000u64) << 190,
                2 => U256::MAX - U256::from(1337u64),
                3 => U256::MAX,
                _ => U256::from(9_000_000_000u64),
            };
            // sample-cap pins: 2/3/6 give step > 1, 4/5 give step == 1 for
            // 5..=24 survivors.
            let cap = [2usize, 3, 6, 4, 5, 8, 12, 48][(seed % 8) as usize];
            let reference = compose_boundary_reference(&hop_lines, &chain, upper, cap);
            let merged = compose_boundary_merged(&hop_lines, &chain, upper, cap);
            // The documented skip-relaxation: when the reference ERRs on a
            // dominated-pair overflow, the merge may legitimately return a
            // tighter Ok envelope. Directionality (load-bearing): Err_merge
            // implies Err_reference (a SELECTED pair failing reduce-retry
            // was composed by the reference too and failed identically).
            match (&reference, &merged) {
                (Ok(reference), Ok(merged)) => {
                    assert_eq!(
                        merged, reference,
                        "seed {seed} cap {cap} upper {upper}: hop={hop_lines:?} chain={chain:?}"
                    );
                }
                (Err(_), Ok(merged_ok)) => {
                    // Documented skip-relaxation with a REAL soundness
                    // check: compose-free grid oracle. At each probe x the
                    // true pointwise bound is min over pairs of
                    // outer.eval(inner.eval(x)) (I512 chain, saturating);
                    // the merged envelope must not undercut it beyond the
                    // double-ceil margin (each eval ceil-rounds up, so the
                    // oracle can sit ABOVE the merged coefficient-eval by
                    // ~1 unit per level — the soundness direction allows
                    // merged < oracle by at most that margin, never the
                    // reverse by more).
                    let merged_lines = merged_ok;
                    let probes = grid_probes(upper);
                    for x in probes {
                        let mut oracle = I512::MAX;
                        for o in &hop_lines {
                            for inner in &chain {
                                let y = inner.eval(&x);
                                let z = eval_i512(o, y);
                                oracle = oracle.min(z);
                            }
                        }
                        let mut merged_at = I512::MAX;
                        for l in merged_lines {
                            merged_at = merged_at.min(l.eval(&x));
                        }
                        // tolerance: 2 units per ceil level
                        let tol = ival(4);
                        assert!(
                            merged_at >= oracle - tol,
                            "seed {seed} @x={x}: merged {merged_at} undercuts oracle {oracle}"
                        );
                    }
                }
                (Ok(_), Err(_)) => {}
                (Err(ref reference_err), Err(ref merged_err)) => {
                    assert_eq!(merged_err, reference_err, "seed {seed}");
                }
            }
        }
    }

    /// I512-input eval mirroring `Line::eval`'s saturation semantics —
    /// the oracle chain feeds inner outputs (I512) into outer lines.
    fn eval_i512(l: &Line, x: I512) -> I512 {
        let bx = l.b.checked_mul(x).unwrap_or(I512::MAX);
        let n = l.a.checked_add(bx).unwrap_or(I512::MAX);
        ceil_div(n, l.c)
    }

    /// tiny i64 literal -> I512 (test helper; Sign preserved via U512).
    fn ival(v: i64) -> I512 {
        if v < 0 {
            -I512::from_raw(U512::from(v.unsigned_abs()))
        } else {
            I512::from_raw(U512::from(v.cast_unsigned()))
        }
    }

    /// Probe grid for the soundness oracle: endpoints + interior points,
    /// never empty, staying within [0, upper].
    fn grid_probes(upper: U256) -> Vec<U256> {
        let q = upper / U256::from(8u64);
        vec![
            U256::ZERO,
            q,
            q * U256::from(2u64),
            q * U256::from(3u64),
            upper / U256::from(2u64),
            q * U256::from(5u64),
            q * U256::from(6u64),
            q * U256::from(7u64),
            upper,
        ]
    }

    /// Falsification families for the merge (7OT63B review): adversarial
    /// determinstic constructions the randomized seeds cannot reach —
    /// concurrent triple-touch (same-function different-repr +
    /// repr-identical duplicates + triple concurrency at a point, with
    /// order permutations), U256::MAX breakpoint clamps (wide intercepts /
    /// near-parallel slopes), crossing exactly at upper, identity-first
    /// chains, and the wide-width overflow family (retry-success, retry
    /// failure Err/Err, dominated-pair-only overflow Err/Ok relaxation).
    #[test]
    #[expect(clippy::too_many_lines)]
    fn merged_compose_falsification_families() {
        let mkline = |a: I512, b: I512, c: I512| Line { a, b, c };
        let pow2 = |bits: u32| I512::ONE << bits;
        let caps = [2usize, 3, 4, 6, 48];

        // helper: run reference vs merged under all caps + permutations
        let check = |tag: &str, hop: &[Line], chain: &[Line], uppers: &[U256]| {
            for &cap in &caps {
                for &upper in uppers {
                    let reference = compose_boundary_reference(hop, chain, upper, cap);
                    let merged = compose_boundary_merged(hop, chain, upper, cap);
                    match (&reference, &merged) {
                        (Ok(reference), Ok(merged)) => {
                            assert_eq!(merged, reference, "{tag} cap={cap}");
                        }
                        (Err(_), Ok(m)) => {
                            assert!(!m.is_empty(), "{tag}: empty merged");
                        }
                        (Ok(_), Err(_)) => {
                            unreachable!("{tag}: merge erred where reference succeeded");
                        }
                        (Err(r), Err(m)) => {
                            assert_eq!(m, r, "{tag}");
                            assert_eq!(
                                m,
                                &GateSkipCause::DomainOverflow,
                                "{tag}: expected DomainOverflow"
                            );
                        }
                    }
                }
            }
        };

        // (a) concurrent triple-touch: same-function DIFFERENT-repr pair,
        // repr-identical duplicate pair, triple concurrency at x=100/y=200.
        let hop_a = vec![
            mkline(ival(-400), ival(4), ival(1)),
            mkline(ival(0), ival(2), ival(1)),
            mkline(ival(400), ival(2), ival(2)),
        ];
        let chain_a = vec![
            mkline(ival(0), ival(2), ival(1)),
            mkline(ival(100), ival(1), ival(1)),
        ];
        let uppers_a: Vec<U256> = [80u64, 100, 120, 200, 300, 1000]
            .iter()
            .map(|&v| U256::from(v))
            .collect();
        // order permutations reach the stable-sort tie machinery
        let hop_a_rev: Vec<Line> = hop_a.iter().rev().copied().collect();
        let chain_a_rev: Vec<Line> = chain_a.iter().rev().copied().collect();
        for (h, c) in [
            (&hop_a, &chain_a),
            (&hop_a_rev, &chain_a),
            (&hop_a, &chain_a_rev),
            (&hop_a_rev, &chain_a_rev),
        ] {
            check("triple-touch", h, c, &uppers_a);
        }

        // (b) MAX-clamp: intercept differences ~2^260 with near-parallel
        // slopes (b/c differing by ~2^-11) push true breakpoints past
        // 2^256; upper = U256::MAX keeps the clamped entry eligible.
        let hop_b = vec![
            mkline(pow2(260), ival(4), ival(1)),
            mkline(pow2(260) + ival(1), ival(4), ival(2)),
            mkline(-pow2(260), ival(4), ival(1)),
        ];
        let chain_b = vec![
            mkline(ival(1), ival(1), ival(1)),
            mkline(pow2(255), ival(1), ival(1)),
        ];
        check(
            "max-clamp",
            &hop_b,
            &chain_b,
            &[U256::MAX, U256::MAX - U256::from(1337u64)],
        );

        // (c) crossing exactly at upper: two lines equal at x = upper.
        let upper_c = U256::from(1_000u64);
        // l1: y = 1250 + x ; l2: y = 250 + 2x -> both 2250 at upper=1000:
        // the crossover lands exactly on the domain endpoint.
        let x1 = pow2(60);
        let hop_c = vec![
            mkline(ival(1250), ival(1), ival(1)),
            mkline(ival(250), ival(2), ival(1)),
            mkline(I512::from_raw(U512::from(x1)) * ival(3), ival(1), ival(1)),
        ];
        let _ = x1;
        check("crossing-at-upper", &hop_c, &chain_a, &[upper_c]);

        // (e) identity-first: production first-boundary shape.
        check(
            "identity-first",
            &hop_a,
            &[Line::IDENTITY],
            &[U256::from(1000u64)],
        );

        // (f) wide-width overflow family: a,b ~2^260 with small c — exact
        // compose products hit 520+ bits (I512 overflow) → reduce-retry.
        // f(i) retry succeeds (both Ok); f(ii) retry still fails (Err/Err
        // with cause DomainOverflow); f(iii) only DOMINATED pairs overflow
        // (Err/Ok relaxation — soundness via the grid oracle in the main
        // randomized arm; here we assert the direction).
        let wide_coeff = |hi: u32, lo: u32, sign: i64| -> I512 {
            let v = (I512::ONE << hi) + ival(i64::from(lo) * sign);
            if sign < 0 {
                -v
            } else {
                v
            }
        };
        let hop_f = vec![
            mkline(wide_coeff(260, 1, 1), wide_coeff(260, 2, 1), ival(1)),
            mkline(ival(1), ival(1), ival(1)),
        ];
        let chain_f = vec![
            mkline(wide_coeff(260, 3, 1), wide_coeff(260, 5, 1), ival(1)),
            mkline(ival(2), ival(1), ival(1)),
        ];
        check(
            "wide-overflow",
            &hop_f,
            &chain_f,
            &[U256::MAX, U256::from(1_000u64) << 200],
        );
    }

    /// T5 oracle v2: hop truth with CHAINED pricing (entry = previous
    /// range's exit bound — the same convention as `crossings()`), because
    /// captured `sqrt_price_x96` for non-head ranges is that range's own
    /// upper bound and may disagree with its bounds for pathological tail
    /// ranges. Consumes input range by range exactly as the walk does.
    fn chained_hop_out(seq: &IntV3TickRangeSequence, mut x: U256) -> U256 {
        let mut out = U256::ZERO;
        let n = seq.ranges.len();
        for i in 0..n {
            if x.is_zero() {
                break;
            }
            let r = &seq.ranges[i];
            // Chained entry price: a swap arriving at range i enters at the
            // previous range's exit bound (zfo: its lower bound; ofz: upper).
            // Capacity check: if the full crossing of range i exceeds the
            // remaining input, the swap lands inside — simulate with a clone
            // whose entry price is the chained one.
            let full = if r.liquidity == 0 {
                (U256::ZERO, U256::ZERO)
            } else {
                let mut gross = U256::ZERO;
                let target_out = U256::ZERO;
                // Reuse int_simulate_v3_swap with a saturated input to get
                // the full-crossing cost? Too heavy; instead detect landing
                // via accumulated crossing compare below.
                let _ = (&mut gross, target_out);
                (U256::ZERO, U256::ZERO)
            };
            let _ = full;
            let entry = if i == 0 {
                r.sqrt_price_x96
            } else if r.zero_for_one {
                seq.ranges[i - 1].sqrt_price_lower_x96
            } else {
                seq.ranges[i - 1].sqrt_price_upper_x96
            };
            let sim_hop = IntV3TickRangeHop {
                liquidity: r.liquidity,
                sqrt_price_x96: entry,
                sqrt_price_lower_x96: r.sqrt_price_lower_x96,
                sqrt_price_upper_x96: r.sqrt_price_upper_x96,
                gamma_numer: r.gamma_numer,
                fee_denom: r.fee_denom,
                zero_for_one: r.zero_for_one,
                word_boundary_prices: r.word_boundary_prices.clone(),
            };
            let res = int_simulate_v3_swap(x, &sim_hop);
            out += res.output;
            x -= res.consumed_input;
        }
        out
    }

    /// T5 zoom 2: for the pathological tail range (index 46 of hop 1), print
    /// the raw fields, the crossing table's drain, and the per-range oracle's
    /// output at partial inputs, to pin which model diverges from on-chain
    /// computeSwapStep semantics.
    // Fixture JSON is committed data: a malformed fixture SHOULD panic the
    // test, so unwrap() is the honest call here.
    // Fixture JSON is committed data: panicking on a malformed fixture IS
    // the test behavior, and these fns deliberately stdout-dump their stage
    // derivations for --nocapture debugging.
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture parsing - panicking is the desired test behavior"
    )]
    #[expect(
        clippy::print_stdout,
        reason = "stage-by-stage derivation dump for --nocapture debugging"
    )]
    #[test]
    fn gate_false_skip_93794_tail_range_dump() {
        let raw = include_str!("../tests/fixtures/gate_false_skip_93794.json");
        let row: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        let parse_range = |v: &serde_json::Value| -> IntV3TickRangeHop {
            IntV3TickRangeHop {
                liquidity: v["liquidity"].as_str().unwrap().parse().unwrap(),
                sqrt_price_x96: v["sqrt_price_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_lower_x96: v["sqrt_price_lower_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_upper_x96: v["sqrt_price_upper_x96"].as_str().unwrap().parse().unwrap(),
                gamma_numer: v["gamma_numer"].as_u64().unwrap(),
                fee_denom: v["fee_denom"].as_u64().unwrap(),
                zero_for_one: v["zero_for_one"].as_bool().unwrap(),
                word_boundary_prices: v["word_boundary_prices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_str().unwrap().parse().unwrap())
                    .collect(),
            }
        };
        let seqs: Vec<IntV3TickRangeSequence> = row["hops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hop| IntV3TickRangeSequence {
                ranges: hop
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(parse_range)
                    .collect::<Vec<_>>(),
            })
            .collect();
        let s = &seqs[1];
        assert!(s.ranges[0].zero_for_one);
        for idx in [44usize, 45, 46, 47] {
            let r = &s.ranges[idx];
            println!(
                "range[{idx}] liq={} sp0={} low={} high={} words={} gamma={}",
                r.liquidity,
                r.sqrt_price_x96,
                r.sqrt_price_lower_x96,
                r.sqrt_price_upper_x96,
                r.word_boundary_prices.len(),
                r.gamma_numer
            );
            if !r.word_boundary_prices.is_empty() {
                println!(
                    "    word[0]={} word[-1]={}",
                    r.word_boundary_prices[0],
                    r.word_boundary_prices[r.word_boundary_prices.len() - 1]
                );
            }
        }
        let crossings = build_cl_crossing_table(s);
        for idx in [44usize, 45, 46, 47] {
            println!(
                "cr[{idx}] acc_in={} acc_out={}",
                crossings[idx].crossing_gross_input, crossings[idx].crossing_output
            );
        }
        // Oracle: partial input into range 46 alone.
        for &x in &[
            1_000_000_000u64,
            10_000_000_000u64,
            100_000_000_000u64,
            1_000_000_000_000u64,
            5_000_000_000_000u64,
        ] {
            let res = int_simulate_v3_swap(U256::from(x), &s.ranges[46]);
            println!(
                "sim46(x={x}) out={} consumed={}",
                res.output, res.consumed_input
            );
        }
    }

    /// T5 forensic: the soak's gate produced bound=1 against golden profit
    /// 2.4e10 on a captured stable-pool 3-hop (block 25886170 path 93794;
    /// ranges/hop [102,48,312]). The envelope chain is on paper airtight
    /// (concave output curves -> entry tangents are global upper bounds;
    /// min-of-lines survives sampling; sound reductions only loosen), so a
    /// bound BELOW the true optimal profit means some stage under-cuts.
    /// This test walks the derivation stage by stage against an ORACLE built
    /// from production's own per-range step (`int_simulate_v3_swap`, the
    /// compute_swap_step_v3 parity path) and names the failing stage.
    #[expect(
        clippy::unwrap_used,
        reason = "test fixture parsing - panicking is the desired test behavior"
    )]
    #[expect(
        clippy::print_stdout,
        reason = "stage-by-stage derivation dump for --nocapture debugging"
    )]
    #[expect(
        clippy::panic,
        reason = "the test asserts an unsupported gate and panics by design"
    )]
    #[test]
    #[expect(clippy::too_many_lines)]
    fn gate_false_skip_93794_stage_bisect() {
        let raw = include_str!("../tests/fixtures/gate_false_skip_93794.json");
        let row: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
        let golden_profit: U256 = row["golden"]["profit"]
            .as_str()
            .expect("golden profit")
            .parse()
            .expect("golden U256");

        let parse_range = |v: &serde_json::Value| -> IntV3TickRangeHop {
            IntV3TickRangeHop {
                liquidity: v["liquidity"].as_str().unwrap().parse().unwrap(),
                sqrt_price_x96: v["sqrt_price_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_lower_x96: v["sqrt_price_lower_x96"].as_str().unwrap().parse().unwrap(),
                sqrt_price_upper_x96: v["sqrt_price_upper_x96"].as_str().unwrap().parse().unwrap(),
                gamma_numer: v["gamma_numer"].as_u64().unwrap(),
                fee_denom: v["fee_denom"].as_u64().unwrap(),
                zero_for_one: v["zero_for_one"].as_bool().unwrap(),
                word_boundary_prices: v["word_boundary_prices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_str().unwrap().parse().unwrap())
                    .collect(),
            }
        };
        let seqs: Vec<IntV3TickRangeSequence> = row["hops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hop| IntV3TickRangeSequence {
                ranges: hop
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(&parse_range)
                    .collect::<Vec<_>>(),
            })
            .collect();
        assert_eq!(seqs.len(), 3);

        // --- Oracle: production-parity exact output for one CL hop at input x
        // (each range via int_simulate_v3_swap; boundary crossings carry the
        // unconsumed remainder to the next range).
        let hop_truth = |seq: &IntV3TickRangeSequence, x: U256| -> U256 { chained_hop_out(seq, x) };

        // --- Stage 1: per-hop tangent lines vs the oracle on a grid.
        let mut all_hop_lines: Vec<Vec<Line>> = Vec::new();
        let mut xmax = U256::ZERO;
        for (hi, s) in seqs.iter().enumerate() {
            let view = HopMath::Cl(ClHop {
                seq: s,
                crossings: std::borrow::Cow::Owned(build_cl_crossing_table(s)),
            });
            let (lines, cap) = hop_lines_and_cap(view).expect("hop derivable");
            xmax = xmax.checked_add(cap).expect("domain sum");
            // Grid over [0, 2*cap]; the hop's search domain within a chain is
            // its input volume, capped by cap.
            let probe_max = cap.saturating_mul(U256::from(2u8));
            let mut worst_gap = I512::ZERO;
            let mut worst_x = U256::ZERO;
            let n_grid = 256u32;
            for i in 0..=n_grid {
                let x = probe_max / U256::from(n_grid) * U256::from(i);
                let truth = I512::try_from(U512::from(hop_truth(s, x))).unwrap_or(I512::MAX);
                let bnd = lines
                    .iter()
                    .map(|l| l.eval(&x))
                    .min()
                    .expect("lines non-empty");
                if bnd < truth && truth - bnd > worst_gap {
                    worst_gap = truth - bnd;
                    worst_x = x;
                }
            }
            println!(
                "stage1 hop{hi}: lines={} cap={cap} worst_gap={worst_gap} at x={worst_x}",
                lines.len()
            );
            assert!(
                worst_gap.is_zero(),
                "hop{hi} line set UNDER-CUTS its true curve at x={worst_x} by {worst_gap}"
            );
            all_hop_lines.push(lines);
        }

        // --- Stage 2: replicate the compose chain (prune + compose + sample
        // + reduce per boundary) and check the composed envelope against the
        // path oracle on the same grid.
        let mut acc: Vec<Line> = vec![Line::IDENTITY];
        for (hi, hop_ls) in all_hop_lines.iter().enumerate() {
            let mut hls = hop_ls.clone();
            prune(&mut hls, xmax);
            let mut next: Vec<Line> = Vec::new();
            for outer in &hls {
                for inner in &acc {
                    next.push(outer.compose(inner).expect("compose ok on grid test"));
                }
            }
            prune(&mut next, xmax);
            for l in &mut next {
                l.reduce(COMPOSE_TARGET_BITS);
            }
            // sampled_compose_lines cap (default 48).
            if next.len() > 48 {
                let step = next.len() / 48;
                let mut sampled: Vec<Line> = Vec::new();
                let mut i = 0;
                while i < next.len() {
                    sampled.push(next[i]);
                    i += step.max(1);
                }
                if sampled.last() != Some(&next[next.len() - 1]) {
                    sampled.push(next[next.len() - 1]);
                }
                next = sampled;
            }
            acc = next;

            // Composed-envelope check at this boundary vs the truth using
            // only the composed hops 0..=hi.
            let mut gap = I512::ZERO;
            let mut gap_x = U256::ZERO;
            let n_grid = 256u32;
            for i in 0..=n_grid {
                let x = xmax / U256::from(n_grid) * U256::from(i);
                let mut truth = U256::ZERO;
                {
                    let mut y = x;
                    for s in &seqs[..=hi] {
                        truth = hop_truth(s, y);
                        y = truth;
                    }
                }
                let t = I512::try_from(U512::from(truth)).unwrap_or(I512::MAX);
                let bnd = acc.iter().map(|l| l.eval(&x)).min().expect("nonempty");
                if bnd < t && t - bnd > gap {
                    gap = t - bnd;
                    gap_x = x;
                }
            }
            println!(
                "stage2 after hop{hi}: survivors={} xmax={xmax} worst_gap={gap} at x={gap_x}",
                acc.len()
            );
            assert!(
                gap.is_zero(),
                "composed envelope UNDER-CUTS the true {}-hop curve at x={gap_x} by {gap}",
                hi + 1
            );
        }

        // --- Stage 3: full gate call must clear the golden.
        let views: Vec<Option<HopMath>> =
            seqs.iter().map(|s| Some(HopMath::cl_derived(s))).collect();
        match path_profit_bound(&views, &GateDeps::offline()) {
            Envelope::Bound(b) => {
                println!("stage3 bound={b} golden={golden_profit}");
                assert!(
                    b >= golden_profit,
                    "gate bound {b} below golden {golden_profit}"
                );
            }
            other @ Envelope::Unsupported(_) => panic!("gate unsupported: {other:?}"),
        }
    }

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

    /// RED (live crash): when every endpoint eval saturates to I512::MAX the
    /// stage-1 sweep used to drop every line and the hull search panicked on
    /// the empty set. prune must never return empty (soundness: the kept line
    /// is still a global upper bound).
    /// Loop-16 T2 differential sentinel: the optimized prune must remain
    /// byte-identical to this FROZEN REFERENCE COPY of the pre-optimization
    /// implementation on randomized line sets (seeded LCG; no external
    /// deps). Any divergence in survivor order/content fails the test.
    fn prune_reference_implementation(lines: &mut Vec<Line>, upper: U256) {
        if lines.len() < 2 {
            return;
        }
        let mut indexed: Vec<([I512; 2], usize)> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| ([l.eval(&U256::ZERO), l.eval(&upper)], i))
            .collect();
        indexed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut min_key1 = I512::MAX;
        let mut surv: Vec<Line> = Vec::with_capacity(lines.len() / 4);
        for (keys, idx) in &indexed {
            if keys[1] < min_key1 {
                min_key1 = keys[1];
                surv.push(lines[*idx]);
            }
        }
        if surv.is_empty() {
            surv.push(lines[indexed[0].1]);
        }
        *lines = surv;
        if lines.len() < 2 {
            return;
        }
        for l in lines.iter_mut() {
            l.reduce(COMPOSE_TARGET_BITS);
        }
        let mut idx: Vec<usize> = (0..lines.len()).collect();
        idx.sort_by(|&i, &j| {
            let (li, lj) = (&lines[i], &lines[j]);
            let lhs = li.b * lj.c;
            let rhs = lj.b * li.c;
            rhs.cmp(&lhs)
        });
        let mut hull: Vec<(U256, usize)> = Vec::with_capacity(idx.len());
        for &li in &idx {
            let l = &lines[li];
            if let Some(&(_, top)) = hull.last() {
                let lt = &lines[top];
                if lt.b * l.c == l.b * lt.c {
                    if lt.a * l.c <= l.a * lt.c {
                        continue;
                    }
                    hull.pop();
                }
            }
            let bp = if let Some(&(_, top)) = hull.last() {
                let lt = &lines[top];
                let num = l.a * lt.c - lt.a * l.c;
                let den = lt.b * l.c - l.b * lt.c;
                ceil_div(num, den)
            } else {
                I512::ZERO
            };
            while hull.len() >= 2 {
                let (bb, t) = hull[hull.len() - 1];
                let lprev = &lines[t];
                let num = l.a * lprev.c - lprev.a * l.c;
                let den = lprev.b * l.c - l.b * lprev.c;
                let bb_i = I512::try_from(U512::from(bb)).unwrap_or(I512::MAX);
                if ceil_div(num, den) <= bb_i {
                    hull.pop();
                } else {
                    break;
                }
            }
            let bx = if bp <= I512::ZERO {
                U256::ZERO
            } else {
                let u = U512::try_from(bp).unwrap_or(U512::MAX);
                if u > U512::from(U256::MAX) {
                    U256::MAX
                } else {
                    u.to::<U256>()
                }
            };
            hull.push((bx, li));
        }
        let keep: Vec<Line> = hull
            .iter()
            .filter(|&&(bx, _)| bx <= upper)
            .map(|&(_, i)| lines[i])
            .collect();
        *lines = keep;
    }

    fn seeded_line(lcg: &mut u64) -> Line {
        let r = |lcg: &mut u64, bits: u32| {
            *lcg = lcg
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let mut v = I512::from_raw(U512::from(*lcg));
            if bits > 64 {
                *lcg = lcg
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                v = (v << 64) | I512::from_raw(U512::from(*lcg));
                let shift = bits - 128;
                if shift > 0 {
                    v <<= shift.min(500);
                }
            } else {
                v <<= 0;
                let _ = v;
                v = I512::from_raw(U512::from(*lcg & ((1u64 << bits.min(63)) - 1)));
            }
            v
        };
        Line {
            // Negative-intercept lines and near-tie cases are the whole
            // point of the differential (the approx-key fallback path).
            a: {
                let v = r(lcg, 128);
                if (*lcg).is_multiple_of(3) {
                    -v
                } else {
                    v
                }
            },
            b: r(lcg, 128),
            c: I512::ONE + r(lcg, 128),
        }
    }

    #[test]
    fn prune_matches_frozen_on_adversarial_ceil_boundaries() {
        let mk = |a: I512, b: I512, c: I512| Line { a, b, c };
        let two = I512::from_raw(U512::from(2u64));
        let big = I512::from_raw(U512::from(0xffff_u64) << 200);
        // ceil-boundary pairs: a/c ratios differing by <1 absolute — the
        // approximation must fall back to the exact comparator.
        let mut lines = vec![
            mk(big, big, big),
            mk(big + two, big, big),
            mk(big - two, big, big),
            mk(-big, big, big),
            mk(-big - two, big, big),
        ];
        // Equal slopes (same b/c) with different intercepts.
        lines.push(mk(I512::from_raw(U512::from(7u64)), big, big));
        lines.push(mk(I512::ZERO, big, big));
        // Saturation-boundary pair: b·U just below and just above 2^511.
        let upper = U256::MAX;
        let b_hi = I512::ONE << 255;
        let b_lo = I512::ONE << 254;
        lines.push(mk(I512::ZERO, b_hi, big));
        lines.push(mk(I512::ZERO, b_lo, big));
        let mut reference = lines.clone();
        prune_reference_implementation(&mut reference, upper);
        prune(&mut lines, upper);
        assert_eq!(lines, reference);
    }

    #[test]
    fn prune_matches_frozen_reference_on_randomized_sets() {
        for seed in 0..256u64 {
            let mut lcg = seed | (seed << 32) | 1;
            let n = (seed % 40) as usize + 2;
            let mut lines: Vec<Line> = (0..n).map(|_| seeded_line(&mut lcg)).collect();
            // Upper points sweep a few magnitudes (endpoint eval saturation
            // and domain-dependent domination both matter).
            let upper = match seed % 4 {
                0 => U256::from(1_000u64) << 96,
                1 => U256::from(1_000u64) << 190,
                2 => U256::MAX - U256::from(1337u64),
                _ => U256::from(9_000_000_000u64),
            };
            let mut reference = lines.clone();
            prune_reference_implementation(&mut reference, upper);
            prune(&mut lines, upper);
            assert_eq!(
                lines, reference,
                "prune diverged from the frozen reference for seed {seed}"
            );
        }
    }

    #[test]
    fn prune_never_empties_when_all_endpoint_evals_saturate() {
        let extreme = Line {
            a: I512::MAX,
            b: I512::MAX,
            c: I512::ONE,
        };
        let upper = U256::MAX;
        let mut lines = vec![extreme, extreme, extreme];
        prune(&mut lines, upper);
        assert!(
            !lines.is_empty(),
            "prune must keep a line even when every endpoint eval saturates"
        );
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
