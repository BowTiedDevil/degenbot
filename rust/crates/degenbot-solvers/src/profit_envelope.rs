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

use alloy::primitives::{aliases::I512, I256, U256, U512};
use degenbot_math::cl::swap_math::compute_swap_step_v3;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};

/// Practical sqrt-price ceiling for bound derivation (`P² ` must leave wide
/// I512 headroom downstream). Real pools sit far below this; above it we
/// decline to derive (caller must not skip) rather than risk overflow.
const P_ENTRY_LIMIT: U256 = U256::from_limbs([0, 0, 1, 0]); // 2^128

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
        self.a = ceil_shr_i512(self.a, k);
        self.b = ceil_shr_i512(self.b, k);
        self.c = (self.c >> k).max(I512::ONE);
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
fn ceil_shr_i512(v: I512, k: u32) -> I512 {
    if k == 0 {
        return v;
    }
    if k >= 512 {
        return if v > I512::ZERO {
            I512::ONE
        } else {
            I512::ZERO
        };
    }
    let shifted = v >> k;
    let mask = (I512::ONE << k).wrapping_sub(I512::ONE);
    let has_remainder = (v & mask) != I512::ZERO;
    if has_remainder {
        shifted + I512::ONE
    } else {
        shifted
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
            let mut lines: Vec<Line> = Vec::with_capacity(seq.ranges.len());
            let mut acc_in = U256::ZERO;
            let mut acc_out = U256::ZERO;
            for k in 0..seq.ranges.len() {
                let cr: IntTickRangeCrossing = seq.compute_crossing(k)?;
                let er = &cr.ending_range;
                let p_entry = er.sqrt_price_x96;
                if p_entry.is_zero() || p_entry >= P_ENTRY_LIMIT {
                    return None;
                }
                let liq = er.liquidity;
                if liq == 0 {
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
                if d512 == U512::ZERO || n512 == U512::ZERO {
                    return None;
                }
                let d = I512::try_from(d512).ok()?;
                let n = I512::try_from(n512).ok()?;
                let oc = I512::try_from(U512::from(acc_out)).ok()?;
                let ic = I512::try_from(U512::from(acc_in)).ok()?;
                let a = oc.checked_mul(d)?.checked_sub(n.checked_mul(ic)?)?;
                lines.push(Line { a, b: n, c: d });
                acc_in = cr.crossing_gross_input;
                acc_out = cr.crossing_output;
            }
            let cr_last = seq.compute_crossing(seq.ranges.len() - 1)?;
            let er = cr_last.ending_range;
            let exit = if er.zero_for_one {
                er.sqrt_price_lower_x96
            } else {
                er.sqrt_price_upper_x96
            };
            let liq = i128::try_from(er.liquidity).ok()?;
            let step = compute_swap_step_v3(
                er.sqrt_price_x96,
                exit,
                liq,
                I256::MAX,
                U256::from(er.fee_denom.saturating_sub(er.gamma_numer)),
            )
            .ok()?;
            // Overflow here just shrinks the search domain conservatively.
            let cap = acc_out.checked_add(step.amount_out)?;
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
/// diagnostic). Mirrors the exact checks in the `HopMath::Cl` arm so the
/// debug log identifies the precise rejection reason + range index.
#[must_use]
fn classify_cl_rejection(seq: &IntV3TickRangeSequence) -> String {
    if seq.ranges.is_empty() {
        return "reject=empty_ranges".to_string();
    }
    for k in 0..seq.ranges.len() {
        let Some(cr) = seq.compute_crossing(k) else {
            return format!("reject=crossing_none@k={k}");
        };
        let er = &cr.ending_range;
        let p = er.sqrt_price_x96;
        if p.is_zero() {
            return format!("reject=zero_price@k={k}");
        }
        if p >= P_ENTRY_LIMIT {
            return format!("reject=price_over_limit@k={k},p_entry_bits={}", p.bit_len());
        }
        if er.liquidity == 0 {
            return format!("reject=zero_liq@k={k}");
        }
        // Check the coefficient overflow path (the marginal-rate U512
        // products + I512 conversions).
        let p_sq = U512::from(p).saturating_mul(U512::from(p));
        let two192 = U512::from(1u8) << 192;
        let (m_num, m_den) = if er.zero_for_one {
            (p_sq, two192)
        } else {
            (two192, p_sq)
        };
        let d512 = U512::from(er.fee_denom).saturating_mul(m_den);
        let n512 = U512::from(er.gamma_numer).saturating_mul(m_num);
        if d512.is_zero() || n512.is_zero() {
            return format!("reject=zero_coeff@k={k}");
        }
        if I512::try_from(d512).is_err() {
            return format!("reject=d512_overflow@k={k},d_bits={}", d512.bit_len());
        }
        if I512::try_from(n512).is_err() {
            return format!("reject=n512_overflow@k={k},n_bits={}", n512.bit_len());
        }
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

    static CAPTURE_COUNT: AtomicU64 = AtomicU64::new(0);
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
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
    {
        let _ = writeln!(f, "{doc}");
    }
}

/// Drop dominated lines: `i` dies if some `j` is `≤ i` at BOTH domain
/// endpoints (affine ⇒ everywhere), breaking ties toward the smaller index so
/// identical lines collapse deterministically. Keeps the bound sound while
/// bounding the line count across chained hops.
fn prune(lines: &mut Vec<Line>) {
    if lines.len() < 2 {
        return;
    }
    let keys: Vec<(I512, I512)> = lines
        .iter()
        .map(|l| (l.eval(&U256::ZERO), l.eval(&U256::ONE)))
        .collect();
    let dominated = |i: usize| -> bool {
        (0..lines.len()).any(|j| {
            let (ki, kj) = (keys[i], keys[j]);
            i != j && kj.0 <= ki.0 && kj.1 <= ki.1 && (kj.0 < ki.0 || kj.1 < ki.1 || j < i)
        })
    };
    let survivors: Vec<Line> = lines
        .iter()
        .copied()
        .enumerate()
        .filter(|(i, _)| !dominated(*i))
        .map(|(_, l)| l)
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
}

thread_local! {
    pub(crate) static GATE_EVALUATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_SKIPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_UNSUPPORTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    // M6776W per-cause None breakdown (advance on the early-return path).
    pub(crate) static GATE_NONE_HOP_UNMAPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_NONE_DEGENERATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_NONE_OVERFLOW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
    GateStats {
        evaluated,
        skipped,
        unsupported,
        none_hop_unmapped,
        none_degenerate,
        none_overflow,
    }
}

/// Rigorous upper bound on `max_x [path_output(x) − x]`, or `None` when any
/// hop is unsupported/degenerate (callers MUST NOT skip on `None`).
#[must_use]
#[expect(clippy::too_many_lines)]
pub fn path_profit_bound(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    let mut lines = vec![Line::IDENTITY];
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
                    let zero_liq = seq.ranges.iter().any(|r| r.liquidity == 0);
                    let reason = classify_cl_rejection(seq);
                    format!(
                        "Cl(ranges={},empty={empty},zero_liq={zero_liq},{reason})",
                        seq.ranges.len()
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
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                // outer(inner(x)): the new line maps path-input x through the
                // chained-so-far line, then through this hop's line.
                let Some(composed) = outer.compose(inner) else {
                    GATE_NONE_OVERFLOW.with(|c| c.set(c.get() + 1));
                    return None;
                };
                next.push(composed);
            }
        }
        prune(&mut next);
        lines = next;
        if let Some(v) = xmax.checked_add(cap) {
            xmax = v;
        } else {
            GATE_NONE_OVERFLOW.with(|c| c.set(c.get() + 1));
            return None;
        }
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
    narrow(f(&lo))
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
        prune(&mut next);
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

    // -----------------------------------------------------------------
    // M6776W degenerate-capture experiment harness: loads captured JSONL
    // paths and tests whether the proposed fixes (zero-liq skip + P_ENTRY_LIMIT
    // widen) recover them to a derivable bound.
    // -----------------------------------------------------------------

    /// An experimental version of the CL `hop_lines_and_cap` arm with
    /// configurable `p_entry_limit` and `skip_zero_liq` — mirrors the
    /// production logic exactly but allows the fix configurations to be
    /// tested against the captured golden state.
    fn cl_hop_lines_and_cap_experimental(
        seq: &IntV3TickRangeSequence,
        skip_zero_liq: bool,
        p_limit: U256,
    ) -> bool {
        if seq.ranges.is_empty() {
            return false;
        }
        let mut lines: Vec<Line> = Vec::with_capacity(seq.ranges.len());
        let mut acc_in = U256::ZERO;
        let mut acc_out = U256::ZERO;
        for k in 0..seq.ranges.len() {
            let Some(cr) = seq.compute_crossing(k) else {
                return false;
            };
            let er = &cr.ending_range;
            let p_entry = er.sqrt_price_x96;
            if p_entry.is_zero() || p_entry >= p_limit {
                return false;
            }
            let liq = er.liquidity;
            if liq == 0 {
                if skip_zero_liq {
                    continue; // FIX 1: skip zero-liq ranges instead of poisoning
                }
                return false;
            }
            // Coefficient derivation (same as production).
            let p_sq = U512::from(p_entry).saturating_mul(U512::from(p_entry));
            let two192 = U512::from(1u8) << 192;
            let (m_num, m_den) = if er.zero_for_one {
                (p_sq, two192)
            } else {
                (two192, p_sq)
            };
            let d512 = U512::from(er.fee_denom).saturating_mul(m_den);
            let n512 = U512::from(er.gamma_numer).saturating_mul(m_num);
            if d512 == U512::ZERO || n512 == U512::ZERO {
                return false;
            }
            let Ok(d) = I512::try_from(d512) else {
                return false;
            };
            let Ok(n) = I512::try_from(n512) else {
                return false;
            };
            let Ok(oc) = I512::try_from(U512::from(acc_out)) else {
                return false;
            };
            let Ok(ic) = I512::try_from(U512::from(acc_in)) else {
                return false;
            };
            let Some(a) = oc
                .checked_mul(d)
                .and_then(|v| v.checked_sub(n.checked_mul(ic)?))
            else {
                return false;
            };
            lines.push(Line { a, b: n, c: d });
            acc_in = cr.crossing_gross_input;
            acc_out = cr.crossing_output;
        }
        // Cap-tail (same as production).
        let Some(cr_last) = seq.compute_crossing(seq.ranges.len() - 1) else {
            return false;
        };
        let er = cr_last.ending_range;
        let exit = if er.zero_for_one {
            er.sqrt_price_lower_x96
        } else {
            er.sqrt_price_upper_x96
        };
        let Ok(liq) = i128::try_from(er.liquidity) else {
            return false;
        };
        let Ok(step) = compute_swap_step_v3(
            er.sqrt_price_x96,
            exit,
            liq,
            I256::MAX,
            U256::from(er.fee_denom.saturating_sub(er.gamma_numer)),
        ) else {
            return false;
        };
        acc_out.checked_add(step.amount_out).is_some() && !lines.is_empty()
    }

    /// Parse one captured JSONL line into a CL `IntV3TickRangeSequence`.
    fn parse_cl_seq(ranges_json: &serde_json::Value) -> Option<IntV3TickRangeSequence> {
        let ranges: Vec<degenbot_pools::int_v3_hop::IntV3TickRangeHop> = ranges_json
            .as_array()?
            .iter()
            .map(|r| {
                Some(degenbot_pools::int_v3_hop::IntV3TickRangeHop {
                    liquidity: r.get("liquidity")?.as_str()?.parse().ok()?,
                    sqrt_price_x96: r.get("sqrt_price_x96")?.as_str()?.parse().ok()?,
                    sqrt_price_lower_x96: r.get("sqrt_price_lower_x96")?.as_str()?.parse().ok()?,
                    sqrt_price_upper_x96: r.get("sqrt_price_upper_x96")?.as_str()?.parse().ok()?,
                    gamma_numer: r.get("gamma_numer")?.as_u64()?,
                    fee_denom: r.get("fee_denom")?.as_u64()?,
                    zero_for_one: r.get("zero_for_one")?.as_bool()?,
                    word_boundary_prices: r
                        .get("word_boundary_prices")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|p| p.as_str()?.parse().ok()).collect())
                        .unwrap_or_default(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(IntV3TickRangeSequence {
            ranges,
            truncated: false,
        })
    }

    /// The experiment: load the captured JSONL, reproduce each rejection
    /// with the current production code, then test the fixes.
    #[test]
    fn m6776w_experiment_fixes_recover_captured_degenerate_paths() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../investigation/captures/gate_degenerate_2026-08-26.jsonl");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return; // capture file not present (CI / dev without captures)
        };

        let mut total = 0u32;
        let mut reproduced_none = 0u32;
        let mut recovered_zero_liq_skip = 0u32;
        let mut recovered_widen_limit = 0u32;
        let mut recovered_both = 0u32;
        let mut still_dead = 0u32;

        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let doc: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
            let reject_hop = doc
                .get("reject_hop")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .try_into()
                .unwrap_or(0);
            let hops = doc
                .get("hops")
                .and_then(|v| v.as_array())
                .expect("hops array");
            let reject_hop_json = &hops[reject_hop];
            let ranges_json = reject_hop_json.get("ranges").expect("ranges");
            if ranges_json.is_null() {
                continue; // V2 or non-CL reject (skip for this experiment)
            }
            let Some(seq) = parse_cl_seq(ranges_json) else {
                continue; // parse failure (skip)
            };
            total += 1;

            // 1. Reproduce the rejection with production code.
            let prod = hop_lines_and_cap(HopMath::Cl(&seq));
            assert!(
                prod.is_none(),
                "path should be degenerate under prod code (line: {line})"
            );
            reproduced_none += 1;

            // 2. Fix 1: skip zero-liq ranges.
            let fix1 = cl_hop_lines_and_cap_experimental(
                &seq,
                true,          // skip_zero_liq
                P_ENTRY_LIMIT, // unchanged limit
            );
            if fix1 {
                recovered_zero_liq_skip += 1;
            }

            // 3. Fix 2: widen P_ENTRY_LIMIT to 2^240.
            let widened_limit = U256::from(1u128) << 240;
            let fix2 = cl_hop_lines_and_cap_experimental(
                &seq,
                false,         // no zero-liq skip
                widened_limit, // widened
            );
            if fix2 {
                recovered_widen_limit += 1;
            }

            // 4. Fix 3: both fixes combined.
            let fix3 = cl_hop_lines_and_cap_experimental(
                &seq,
                true,          // skip_zero_liq
                widened_limit, // widened
            );
            if fix3 {
                recovered_both += 1;
            } else {
                still_dead += 1;
            }
        }

        #[expect(clippy::print_stderr)]
        {
            eprintln!(
                "M6776W experiment: total={total} reproduced_none={reproduced_none} \
                 fix1(zero_liq_skip)={recovered_zero_liq_skip} \
                 fix2(widen_2^240)={recovered_widen_limit} \
                 fix3(both)={recovered_both} \
                 still_dead={still_dead}"
            );
        }
        // Both fixes together must recover the vast majority (only the
        // genuinely-dead all-zero-liq pools should remain).
        assert!(
            recovered_both >= total * 4 / 5,
            "combined fix should recover >=80% of captured paths, got {recovered_both}/{total}"
        );
    }

    /// Verify the 8 captured dead paths all have zero active liquidity in
    /// EVERY range — the shape the tighter `swap_is_viable` catches.
    /// A range with `liquidity = 0` means either `slot0.liquidity = 0` (base)
    /// or the cumulative `liquidity_net` sum is zero at that range. If ALL
    /// ranges have `liquidity = 0`, every `liquidity_net` at every initialized
    /// tick in the walk direction is zero — the pool is genuinely dead.
    #[test]
    fn m6776w_captured_dead_paths_all_have_zero_range_liquidity() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../investigation/captures/gate_degenerate_2026-08-26.jsonl");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return; // capture file not present
        };

        let mut dead_paths = 0u32;
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let doc: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
            let reason = doc
                .get("reject_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if !reason.contains("zero_liq") {
                continue;
            }
            let reject_hop = doc
                .get("reject_hop")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .try_into()
                .unwrap_or(0);
            let hops = doc
                .get("hops")
                .and_then(|v| v.as_array())
                .expect("hops array");
            let ranges = hops[reject_hop]
                .get("ranges")
                .and_then(|v| v.as_array())
                .expect("ranges array");

            // Check: does ANY range have non-zero liquidity?
            // If all ranges have liquidity=0, tighter swap_is_viable
            // would reject this pool at resolve time (catching it upstream).
            let all_zero = ranges
                .iter()
                .all(|r| r.get("liquidity").and_then(|v| v.as_str()) == Some("0"));

            if all_zero {
                dead_paths += 1;
            } else {
                // A zero_liq rejection on a pool that has SOME non-zero
                // ranges is the recoverable case (the skip-zero-liq fix
                // handles it) — NOT the dead pool shape.
            }
        }

        // All zero_liq rejections that are all-zero-liquidity are genuinely
        // dead (the tighter check would catch them).
        // From the analysis: 8 paths are all-zero (8 ticks, all net=0),
        // 13 have non-zero ranges (recoverable by skipping zero-liq ranges).
        assert_eq!(
            dead_paths, 8,
            "expected exactly 8 all-zero-liquidity dead paths"
        );
    }
}
