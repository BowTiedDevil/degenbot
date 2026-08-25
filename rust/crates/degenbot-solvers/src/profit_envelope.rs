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
    fn compose(&self, inner: &Self) -> Option<Self> {
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

/// Ceiling division for `d > 0` (truncation-toward-zero makes negatives exact).
fn ceil_div(n: I512, d: I512) -> I512 {
    debug_assert!(d > I512::ZERO);
    if n >= I512::ZERO {
        (n + (d - I512::ONE)) / d
    } else {
        n / d
    }
}

/// What the gate needs from one resolved hop. `None` slots (unsupported
/// families) poison the whole path: no skip without a rigorous bound.
#[derive(Clone, Copy, Debug)]
pub enum HopMath<'a> {
    /// Constant-product hop (exact Möbius family: V2/Aerodrome-style state).
    V2(&'a IntHopState),
    /// Concentrated-liquidity hop (ordered tick-range sequence, swap direction).
    Cl(&'a IntV3TickRangeSequence),
}

/// Affine lines dominating one hop's output curve, plus the hop's maximum
/// extractable output (used to cap the search domain).
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
}

thread_local! {
    pub(crate) static GATE_EVALUATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_SKIPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static GATE_UNSUPPORTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Reset all gate counters on the calling thread (call at solve-cycle start,
/// mirroring [`crate::mobius_v3_int::reset_walk_stats`]).
pub fn reset_gate_stats() {
    GATE_EVALUATED.with(|c| c.set(0));
    GATE_SKIPPED.with(|c| c.set(0));
    GATE_UNSUPPORTED.with(|c| c.set(0));
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
    GateStats {
        evaluated,
        skipped,
        unsupported,
    }
}

/// Rigorous upper bound on `max_x [path_output(x) − x]`, or `None` when any
/// hop is unsupported/degenerate (callers MUST NOT skip on `None`).
#[must_use]
pub fn path_profit_bound(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    let mut lines = vec![Line::IDENTITY];
    let mut xmax = U256::ZERO;
    for slot in hops {
        let hop = slot.as_ref()?;
        let (hop_ls, cap) = hop_lines_and_cap(*hop)?;
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                // outer(inner(x)): the new line maps path-input x through the
                // chained-so-far line, then through this hop's line.
                next.push(outer.compose(inner)?);
            }
        }
        prune(&mut next);
        lines = next;
        xmax = xmax.checked_add(cap)?;
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
    let (_, mag512) = v.into_sign_and_abs();
    let mag = mag512.as_limbs();
    if mag[4] != 0 || mag[5] != 0 || mag[6] != 0 || mag[7] != 0 {
        return None;
    }
    Some(U256::from_limbs([mag[0], mag[1], mag[2], mag[3]]))
}
#[must_use]
pub fn path_output_bound_at(hops: &[Option<HopMath<'_>>], x: &U256) -> Option<U256> {
    let mut lines = vec![Line::IDENTITY];
    for slot in hops {
        let hop = slot.as_ref()?;
        let (hop_ls, _cap) = hop_lines_and_cap(*hop)?;
        let mut next: Vec<Line> = Vec::with_capacity(lines.len() * hop_ls.len());
        for outer in &hop_ls {
            for inner in &lines {
                next.push(outer.compose(inner)?);
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
