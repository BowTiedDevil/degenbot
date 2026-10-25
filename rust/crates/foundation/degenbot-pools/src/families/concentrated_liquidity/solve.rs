//! The concentrated-liquidity family module (ADR-071 D1/D2).
//!
//! GLOSSARY.md "Family module": the solve tree's single module owning
//! everything the concentrated liquidity hop family contributes to a solve
//! and its bound. concentrated liquidity is the extreme case: V3 and V4
//! share one solve (ADR-069), so both project to the same gate view and walk
//! the one walkable-path entry.
//!
//! What lives here:
//! - the walkable-path solve entry ([`solve_walkable_path_int`]) — the
//!   family's path-level solve, registered on its `FAMILY_DISPATCH` row;
//! - the gate projection ([`concentrated_liquidity_to_math`]) — V3/V4 →
//!   `HopMath::ConcentratedLiquidity`;
//! - the family's envelope lines ([`concentrated_liquidity_hop_lines`]) — the tangent fan derived
//!   from the hop's carried crossing table, plus its pool-static fan/key
//!   derivation.
//!
//! The engine-owned fan cache ([`PrefixCache`]) stays engine-owned and is
//! borrowed through the interface: this module derives fans and keys, the
//! cache stores them. The kernel (`crate::solve::concentrated_liquidity`) carries no envelope
//! vocabulary; [`refine_bounds`] is the seam that translates the composed
//! `PathBoundLines` into the kernel's own refine-window data.

use std::sync::Arc;

use alloy::primitives::{aliases::I512, U256, U512};

use crate::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};

use crate::solve::concentrated_liquidity::{ConcentratedLiquidityCrossingTable, RefineBound};
use crate::solve::mixed::{ResolvedHop, ResolvedMixedPath, SolvePathResult};
use crate::solve::profit_envelope::{
    content_mix_u256, record_gate, GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache,
};
use crate::solve::runtime::SolveRuntimeConfig;

/// One concentrated liquidity hop's gate view: the sequence plus its crossing table.
#[derive(Clone, Debug)]
pub struct ConcentratedLiquidityHop<'a> {
    pub seq: &'a IntV3TickRangeSequence,
    /// Borrowed from the resolve pass's Arc table in production; owned
    /// (built once via the production builder) in the derived convenience.
    pub crossings: std::borrow::Cow<'a, ConcentratedLiquidityCrossingTable>,
}

/// The concentrated-liquidity family's gate projection: V3 and V4 share one
/// prepared hop (ADR-069), so both resolve to `HopMath::ConcentratedLiquidity` (ADR-071 D2).
pub(crate) fn concentrated_liquidity_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::V3 { prepared, .. } | ResolvedHop::V4 { prepared, .. } => {
            Some(HopMath::ConcentratedLiquidity(ConcentratedLiquidityHop {
                seq: &prepared.int_seq,
                crossings: std::borrow::Cow::Borrowed(&prepared.crossings),
            }))
        }
        _ => None,
    }
}

/// Translate the envelope's composed bound lines into the kernel's own
/// refine-window vocabulary at the seam. The concentrated liquidity family module is the bound
/// side; the kernel names no envelope type.
#[must_use]
pub(crate) fn refine_bounds(lines: &[Line]) -> Vec<RefineBound> {
    lines
        .iter()
        .map(|l| RefineBound {
            a: l.a,
            b: l.b,
            c: l.c,
        })
        .collect()
}

/// Solve a walkable path — every hop is V2, V3, or V4 — with the active-set
/// piecewise Möbius walk.
///
/// All-concentrated-liquidity and mixed V2+concentrated liquidity paths share
/// this ONE construction flow: one borrowed hop view per position (V2 state
/// or prepared concentrated liquidity hop) into the one solve body — nothing
/// cloned, derived, or rebuilt per solve.
///
/// The pre-built `IntV3TickRangeSequence` from `resolve_path` was constructed
/// directly from U256 values (no f64 conversion); V3 and V4 hops produce the
/// same type.
pub(crate) fn solve_walkable_path_int(
    resolved: &ResolvedMixedPath,
    gate: &GateDeps<'_>,
    walk_env: Option<&PathBoundLines>,
) -> crate::solve::mixed::SolveArmOutcome {
    if resolved.hops.len() < 2 {
        return crate::solve::mixed::SolveArmOutcome::refused(
            crate::solve::concentrated_liquidity::WalkRefusal::TooShort,
        );
    }

    // Translate the envelope's composed bound lines into the kernel's own
    // refine-window vocabulary at the seam (the family module is the bound
    // side; the kernel names no envelope type).
    let window = walk_env.map(|env| refine_bounds(&env.lines));

    // ONE route for every path this arm accepts (all-concentrated-liquidity included): one
    // borrowed hop view per position — V2 borrows the resolved state; concentrated liquidity
    // borrows the projection's prepared value (the tick-range sequence paired
    // with its `Arc` tables), so the positional pairing is structural and
    // nothing is cloned, derived, or rebuilt per solve. The all-concentrated-liquidity dispatch
    // branch this replaces cloned the prepared hops (three `Arc` bumps per
    // hop) into a second solve body; the intake shape survives only as the
    // forwarding `solve_concentrated_liquidity_piecewise` convenience for offline callers.
    let mut hops: Vec<crate::solve::concentrated_liquidity::MixedHopView<'_>> =
        Vec::with_capacity(resolved.hops.len());
    for h in &resolved.hops {
        match h {
            ResolvedHop::V2 { state } => {
                hops.push(crate::solve::concentrated_liquidity::MixedHopView::V2(
                    state,
                ));
            }
            ResolvedHop::V3 { prepared } | ResolvedHop::V4 { prepared } => {
                hops.push(
                    crate::solve::concentrated_liquidity::MixedHopView::ConcentratedLiquidity(
                        prepared,
                    ),
                );
            }
            // Not a walkable V2/concentrated liquidity path (the caller routes other families
            // elsewhere); refuse rather than solve a partial composition.
            _ => {
                return crate::solve::mixed::SolveArmOutcome::refused(
                    crate::solve::concentrated_liquidity::WalkRefusal::NotWalkable,
                )
            }
        }
    }

    let out = crate::solve::concentrated_liquidity::solve_piecewise(
        &hops,
        gate.walk_memo(),
        gate.telemetry(),
        &gate.runtime,
        window.as_deref(),
    );

    crate::solve::mixed::SolveArmOutcome {
        result: out.result.map(|(optimal_input, profit, hop_outputs)| {
            // consumed_inputs[0] = optimal_input (first hop consumes full input).
            // consumed_inputs[i>0] = hop_outputs[i-1] (previous hop's output
            // becomes this hop's input).
            let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
            consumed_inputs.push(optimal_input);
            for i in 1..hop_outputs.len() {
                consumed_inputs.push(hop_outputs[i - 1]);
            }
            SolvePathResult {
                optimal_input,
                profit,
                hop_outputs,
                consumed_inputs,
                ..Default::default()
            }
        }),
        refusal: out.refusal,
        stats: out.stats,
    }
}

// ---------------------------------------------------------------------------
// concentrated liquidity envelope lines: the tangent fan + its pool-static cache key
// ---------------------------------------------------------------------------

/// The first eligible crossing's tangent: the only concentrated liquidity line that anchors at the
/// table's live in-range price, so it is derived per call and never cached.
enum ConcentratedLiquidityHead {
    /// No crossing carried liquidity (the table is degenerate).
    Absent,
    /// The first liquid crossing's tangent, if its coefficients fit `I512`.
    Tangent(Option<Line>),
    /// A liquid crossing had a zero entry price: hard reject, no bound.
    ZeroPrice,
}

/// Locate and derive the head tangent (see [`ConcentratedLiquidityHead`]).
fn concentrated_liquidity_head(crossings: &[IntTickRangeCrossing]) -> ConcentratedLiquidityHead {
    for cr in crossings {
        let er = &cr.ending_range;
        if er.liquidity == 0 {
            continue;
        }
        if er.sqrt_price_x96.is_zero() {
            return ConcentratedLiquidityHead::ZeroPrice;
        }
        return ConcentratedLiquidityHead::Tangent(concentrated_liquidity_tangent_from_crossing(
            cr,
        ));
    }
    ConcentratedLiquidityHead::Absent
}

/// One crossing's entry-slope tangent line. `None` on a zero
/// denominator/numerator or an `I512` coefficient overflow — the caller drops
/// the line, which only loosens the envelope (every tangent is a global upper
/// bound).
fn concentrated_liquidity_tangent_from_crossing(cr: &IntTickRangeCrossing) -> Option<Line> {
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
fn concentrated_liquidity_keep_selection(
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
        // the envelope at both domain endpoints). The concentrated liquidity tangents sit on the
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

/// Pool-static concentrated liquidity tangent set: every sampled line after the head, plus the
/// asymptotic cap from the last crossing. A pure function of the crossing
/// table (the head alone anchors at the live price), so it is cacheable by
/// content.
#[derive(Clone, Debug)]
pub(crate) struct ConcentratedLiquidityFan {
    lines: Vec<Line>,
    cap: U256,
}

/// Derive the pool-static fan (see [`ConcentratedLiquidityFan`]). `None` is a hard reject (a
/// liquid crossing carried a zero entry price, or the table is empty) and is
/// never cached.
fn derive_concentrated_liquidity_fan(
    crossings: &[IntTickRangeCrossing],
    sel: &[usize],
) -> Option<ConcentratedLiquidityFan> {
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
            if let Some(l) = concentrated_liquidity_tangent_from_crossing(cr) {
                lines.push(l);
            }
        }
        keep_idx += 1;
    }
    let cap = concentrated_liquidity_cap_tail(crossings.last()?);
    Some(ConcentratedLiquidityFan { lines, cap })
}

/// Asymptotic output of the LAST range (cross it fully with infinite input),
/// added to its accumulated anchor. Mirrors `exact_in_step_to_target`'s output
/// rounding (round DOWN) while accepting u128 liquidity that overflows i128.
fn concentrated_liquidity_cap_tail(cr_last: &IntTickRangeCrossing) -> U256 {
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

/// concentrated liquidity hop lines + domain cap. The head tangent (live in-range price) is
/// derived per call; the pool-static fan after it is reused from `fan_cache`
/// when present, keyed by the fan's content hash and generationed by the
/// solve-cycle epoch.
fn concentrated_liquidity_lines_and_cap(
    crossings: &[IntTickRangeCrossing],
    max_tangent_lines: usize,
    mass: bool,
    fan_cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    if crossings.is_empty() {
        return None;
    }
    let head = match concentrated_liquidity_head(crossings) {
        ConcentratedLiquidityHead::ZeroPrice => return None,
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
    let (n_keeps, sel) = concentrated_liquidity_keep_selection(crossings, max_tangent_lines, mass);
    let fan = if let Some((store, epoch)) = fan_cache {
        let key = concentrated_liquidity_fan_key(crossings, n_keeps, &sel, max_tangent_lines, mass);
        if let Some(hit) = store.get_concentrated_liquidity_fan(epoch, &key) {
            record_gate(stats, |t| t.concentrated_liquidity_fan_hits += 1);
            hit
        } else {
            record_gate(stats, |t| t.concentrated_liquidity_fan_misses += 1);
            // A hard-reject fan is never cached (no poisoned entries).
            let derived = Arc::new(derive_concentrated_liquidity_fan(crossings, &sel)?);
            store.insert_concentrated_liquidity_fan(epoch, key, Arc::clone(&derived));
            derived
        }
    } else {
        Arc::new(derive_concentrated_liquidity_fan(crossings, &sel)?)
    };

    let mut lines: Vec<Line> = Vec::with_capacity(1 + fan.lines.len());
    if let ConcentratedLiquidityHead::Tangent(Some(l)) = head {
        lines.push(l);
    }
    lines.extend_from_slice(&fan.lines);
    if lines.len() > max_tangent_lines {
        lines = sample_tangent_lines(&lines, max_tangent_lines);
    }
    // Every range was zero-liquidity (or every tangent overflowed) → genuinely
    // dead pool. Reject as degenerate so classify_concentrated_liquidity_rejection can report
    // `all_zero_liq`.
    if lines.is_empty() {
        return None;
    }
    Some((lines, fan.cap))
}

/// The concentrated-liquidity family's line builder: the tangent fan derived
/// from the hop's carried crossing table.
pub(crate) fn concentrated_liquidity_hop_lines(
    hop: &HopMath<'_>,
    cfg: &SolveRuntimeConfig,
    fan_cache: Option<(&PrefixCache, u64)>,
    stats: &mut GateStats,
) -> Option<(Vec<Line>, U256)> {
    let HopMath::ConcentratedLiquidity(ch) = hop else {
        return None;
    };
    let seq = ch.seq;
    // Tangent-line budget: dense concentrated liquidity pools emit one tangent per range,
    // and composition across two concentrated liquidity hops multiplies them (K² not
    // R1×R2) — so the set is sampled to `max_tangent_lines`. Keeping
    // fewer tangents only LOOSENS the envelope (every tangent is a
    // global upper bound), so the gate never skips a profitable path.
    let max_tangent_lines = cfg.max_tangent_lines.max(1);
    if seq.ranges.is_empty() {
        return None;
    }
    // Carried crossings: the table the resolve pass already built once
    // per (pool, direction) for the active-set walk — deriving it here
    // per path dominated gate time. The table rides the [ConcentratedLiquidityHop]
    // descriptor; tableless callers pay their own derive via
    // HopMath::concentrated_liquidity_derived.
    let crossings: &[IntTickRangeCrossing] = ch.crossings.as_ref();
    concentrated_liquidity_lines_and_cap(
        crossings,
        max_tangent_lines,
        cfg.tangent_sample_by_mass,
        fan_cache,
        stats,
    )
}

/// Static-content key for a concentrated liquidity hop's pool-static tangent fan. The fan is a
/// function of the SELECTED crossings only (the head anchors at the live
/// in-range price and is rebuilt per call), so the key hashes just those
/// entries' derivation inputs plus the cap-tail inputs — re-hashing the whole
/// table per path would rival the derivation it saves. `n_keeps`/`sel.len()`
/// pin the sampling shape; the stance fields keep two configs from sharing a
/// fan.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ConcentratedLiquidityFanKey {
    table: u128,
    max_tangent_lines: usize,
    mass: bool,
}

/// Mix everything `concentrated_liquidity_tangent_from_crossing` reads.
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

/// Mix everything `concentrated_liquidity_cap_tail` reads.
fn mix_cap_inputs(mut h: u128, cr: &IntTickRangeCrossing) -> u128 {
    let er = &cr.ending_range;
    h = content_mix_u256(h, &cr.crossing_output);
    h = content_mix_u256(h, &er.sqrt_price_x96);
    h = content_mix_u256(h, &er.sqrt_price_lower_x96);
    h = content_mix_u256(h, &er.sqrt_price_upper_x96);
    (h ^ er.liquidity ^ u128::from(u64::from(er.zero_for_one))).wrapping_mul(0x0000_0100_0000_01B3)
}

/// Build the fan key from the precomputed selection (`n_keeps`, `sel`).
fn concentrated_liquidity_fan_key(
    crossings: &[IntTickRangeCrossing],
    n_keeps: usize,
    sel: &[usize],
    max_tangent_lines: usize,
    mass: bool,
) -> ConcentratedLiquidityFanKey {
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
    ConcentratedLiquidityFanKey {
        table: h,
        max_tangent_lines,
        mass,
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    use crate::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeHop};
    use alloy::primitives::U256;
    use degenbot_math::concentrated_liquidity::tick_math::get_sqrt_ratio_at_tick_internal;

    use crate::solve::concentrated_liquidity::build_concentrated_liquidity_crossing_table;
    use crate::solve::profit_envelope::{GateStats, HopMath, Line, PrefixCache};
    use crate::solve::runtime::SolveRuntimeConfig;

    fn sp_at(tick: i32) -> U256 {
        U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap_or_default())
    }

    /// Dense zero-for-one concentrated liquidity sequence: `n` contiguous 1-tick ranges below the
    /// anchor, all with `liq` — a fat table that trips the tangent sample cap.
    fn fat_concentrated_liquidity_seq(n: i32, liq: u128) -> IntV3TickRangeSequence {
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
    fn concentrated_liquidity_fan_cache_is_byte_identical_cold_warm_and_uncached() {
        let seq = fat_concentrated_liquidity_seq(200, 1_000_000_000_000);
        let mk = || {
            HopMath::ConcentratedLiquidity(ConcentratedLiquidityHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(build_concentrated_liquidity_crossing_table(
                    &seq,
                )),
            })
        };
        for mass in [false, true] {
            let cfg = SolveRuntimeConfig {
                tangent_sample_by_mass: mass,
                ..SolveRuntimeConfig::default()
            };
            let (reference, reference_cap) =
                concentrated_liquidity_hop_lines(&mk(), &cfg, None, &mut GateStats::default())
                    .expect("derivable");
            assert!(
                reference.len() < seq.ranges.len(),
                "sample cap must drop most of the 200-range table"
            );

            let store = PrefixCache::new();
            let mut stats = GateStats::EMPTY;
            let (cold, cold_cap) =
                concentrated_liquidity_hop_lines(&mk(), &cfg, Some((&store, 1)), &mut stats)
                    .expect("derivable");
            assert_eq!(
                stats.concentrated_liquidity_fan_misses, 1,
                "cold must miss once"
            );
            assert_eq!(stats.concentrated_liquidity_fan_hits, 0);

            let mut stats = GateStats::EMPTY;
            let (warm, warm_cap) =
                concentrated_liquidity_hop_lines(&mk(), &cfg, Some((&store, 1)), &mut stats)
                    .expect("derivable");
            assert_eq!(
                stats.concentrated_liquidity_fan_hits, 1,
                "warm must hit once"
            );
            assert_eq!(stats.concentrated_liquidity_fan_misses, 0);

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
    fn concentrated_liquidity_fan_key_ignores_head_price_and_tracks_tick_set() {
        let seq = fat_concentrated_liquidity_seq(80, 1_000_000_000_000);
        let cfg = SolveRuntimeConfig::default();
        let base = build_concentrated_liquidity_crossing_table(&seq);
        let view = |crossings: &[IntTickRangeCrossing]| {
            HopMath::ConcentratedLiquidity(ConcentratedLiquidityHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(crossings.to_vec()),
            })
        };
        let key_of = |crossings: &[IntTickRangeCrossing]| {
            let (n, sel) =
                concentrated_liquidity_keep_selection(crossings, cfg.max_tangent_lines, false);
            concentrated_liquidity_fan_key(crossings, n, &sel, cfg.max_tangent_lines, false)
        };

        let store = PrefixCache::new();
        let (base_lines, _) = concentrated_liquidity_hop_lines(
            &view(&base),
            &cfg,
            Some((&store, 1)),
            &mut GateStats::default(),
        )
        .expect("derivable");

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
            concentrated_liquidity_hop_lines(&view(&moved), &cfg, Some((&store, 1)), &mut stats)
                .expect("derivable");
        assert_eq!(
            stats.concentrated_liquidity_fan_hits, 1,
            "price move must hit"
        );
        assert_eq!(stats.concentrated_liquidity_fan_misses, 0);
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
            concentrated_liquidity_hop_lines(&view(&changed), &cfg, Some((&store, 1)), &mut stats)
                .expect("derivable");
        assert_eq!(
            stats.concentrated_liquidity_fan_misses, 1,
            "changed table must miss"
        );
    }

    /// A hard-reject fan (zero entry price deeper in the table) must not write
    /// a cache entry, so a subsequent valid lookup cannot be poisoned.
    #[test]
    fn concentrated_liquidity_fan_reject_does_not_poison_cache() {
        let seq = fat_concentrated_liquidity_seq(80, 1_000_000_000_000);
        let cfg = SolveRuntimeConfig::default();
        let store = PrefixCache::new();
        let view = |crossings: &[IntTickRangeCrossing]| {
            HopMath::ConcentratedLiquidity(ConcentratedLiquidityHop {
                seq: &seq,
                crossings: std::borrow::Cow::Owned(crossings.to_vec()),
            })
        };
        let mut poisoned = build_concentrated_liquidity_crossing_table(&seq);
        poisoned[1].ending_range.sqrt_price_x96 = U256::ZERO;
        let poisoned_view = view(&poisoned);
        assert!(
            concentrated_liquidity_hop_lines(
                &poisoned_view,
                &cfg,
                Some((&store, 1)),
                &mut GateStats::default()
            )
            .is_none(),
            "zero entry price must be a hard reject"
        );

        let valid = build_concentrated_liquidity_crossing_table(&seq);
        let mut stats = GateStats::EMPTY;
        let hit =
            concentrated_liquidity_hop_lines(&view(&valid), &cfg, Some((&store, 1)), &mut stats);
        assert!(hit.is_some(), "the valid table must still derive");
        assert_eq!(
            stats.concentrated_liquidity_fan_misses, 1,
            "nothing was cached by the reject"
        );
    }
}
