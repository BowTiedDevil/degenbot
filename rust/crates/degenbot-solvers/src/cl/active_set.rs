use std::sync::Arc;

use hashbrown::HashSet;

use alloy::primitives::U256;

use degenbot_core::op_warn;
use degenbot_math::v2::IntHopState;

use super::crossings::{
    landed_ending_range_index, piece_window_left_edge, piece_window_right_edge,
    piece_window_right_edge_evented,
};
use super::hop_sim::int_simulate_v3_swap;
use super::telemetry::{
    event_census_record, peek_walk_stats, reset_walk_stats, Mark, WalkEventCensus,
    EVENT_CENSUS_PIECES, WALK_ANCHOR_ARGMAX_NS, WALK_ANCHOR_BUILD_NS, WALK_ANCHOR_COMPOSE_NS,
    WALK_ANCHOR_NS_TOTAL, WALK_ANCHOR_SIMS, WALK_CENSUS_DIR_NS, WALK_CENSUS_DIR_SIMNS,
    WALK_CENSUS_DIR_SIMS, WALK_CENSUS_EDGE_NS, WALK_CENSUS_EDGE_SIMNS, WALK_CENSUS_EDGE_SIMS,
    WALK_CENSUS_REDGE_NS, WALK_CENSUS_REDGE_SIMNS, WALK_CENSUS_REDGE_SIMS, WALK_CENSUS_REFINE_NS,
    WALK_CENSUS_REFINE_SIMNS, WALK_CENSUS_REFINE_SIMS, WALK_GRID_SIMS, WALK_PATH_SIMULATIONS,
    WALK_PIECES_VISITED, WALK_REFINE_SIMS, WALK_SIM_NS_TOTAL, WALK_SOLVE_NS_TOTAL,
    WALK_TERNARY_SIMS,
};
use super::{ClCrossingTable, ClProfileTable};
use crate::runtime::{AnchorSweep, SolveRuntimeConfig};

// ---------------------------------------------------------------------------
// Active-set piecewise Möbius walk
// ---------------------------------------------------------------------------
//
// The path profit function `P(x) = O(x) − x` over any mix of constant-product
// (V2) and concentrated-liquidity (V3/V4) hops is concave, C¹ (the spot price
// is continuous across tick crossings; a liquidity change moves only the
// second derivative), and piecewise Möbius in the path input `x`. The piece
// containing the argmax is therefore found by a MONOTONE walk over pieces —
// no combinatorial enumeration of ending-range tuples and no `max_candidates`
// prefix cap. See
// `docs/architecture/mobius_v3_ending_range_enumeration_evaluation.md`.

/// A hop in the active-set walk: constant-product (a single piece — no tick
/// ranges; V2-family) or concentrated-liquidity (one piece per ending range).
pub(super) enum WalkHop<'a> {
    /// V2-family constant-product hop. The landed tuple entry is always 0.
    ConstantProduct(&'a IntHopState),
    /// CL hop with its pre-computed per-index crossing table (`crossings[k]`
    /// is [`IntV3TickRangeSequence::compute_crossing`]`(k)`).
    Cl {
        /// Crossing data for every ending-range index `k` in `0..ranges.len()`.
        /// `Arc`-backed so the projection's precomputed table is shared - not
        /// re-cloned - across every path reusing the hop.
        crossings: Arc<ClCrossingTable>,
        /// Optional precomputed forward word-boundary profile per `crossings[k]`
        /// (dense ranges only; `None` keeps those on the linear walk). Parallel to
        /// `crossings`. `Arc`-backed so the projection's precomputed profile (on
        /// the [`crate::mixed::ResolvedHop`]) is shared - not re-cloned - across
        /// every path reusing the hop (the hop-projection memoization).
        profiles: Arc<ClProfileTable>,
    },
}

/// Result of a [`simulate_walk_path`] evaluation: the per-hop outputs plus
/// the ending-range tuple the input actually landed in.
pub(super) struct WalkPathOutcome {
    /// Output after the last hop.
    pub(super) final_output: U256,
    /// `hop_outputs[i]` = output after hop `i`.
    pub(super) hop_outputs: Vec<U256>,
    /// Ending-range index landed in per hop (always 0 for V2 hops).
    pub(super) landed: Vec<usize>,
}

/// Simulate the path with SELF-DETERMINED crossings: each CL hop's ending
/// range is derived from the gross input actually available at that hop
/// (hop 0: `amount_in`; hop i: hop `i−1`'s output).
///
/// Unlike `int_simulate_cl_path_n` (which simulates under an ASSUMED crossing
/// tuple and returns the zero-exhaustion shape when the input cannot afford
/// the assumption), this walker always simulates the piece the input truly
/// lands in — which is what makes it usable as the walk's ground truth for
/// any candidate.
pub(super) fn simulate_walk_path(amount_in: U256, hops: &[WalkHop]) -> WalkPathOutcome {
    WALK_PATH_SIMULATIONS.with(|c| c.set(c.get() + 1));
    let sim_t0 = std::time::Instant::now();
    let out = simulate_walk_path_inner(amount_in, hops);
    WALK_SIM_NS_TOTAL.fetch_add(
        u64::try_from(sim_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    out
}

fn simulate_walk_path_inner(amount_in: U256, hops: &[WalkHop]) -> WalkPathOutcome {
    let n_hops = hops.len();
    let mut hop_outputs = Vec::with_capacity(n_hops);
    let mut landed = Vec::with_capacity(n_hops);
    let mut current = amount_in;

    for hop in hops {
        if current.is_zero() {
            hop_outputs.push(U256::ZERO);
            landed.push(0);
            continue;
        }
        match hop {
            WalkHop::ConstantProduct(hop_state) => {
                landed.push(0);
                let out = match hop_state.swap(current) {
                    Ok(o) => o,
                    // V2 hop overflow-reverts on-chain → path yields nothing.
                    Err(_) => U256::ZERO,
                };
                hop_outputs.push(out);
                current = out;
            }
            WalkHop::Cl {
                crossings,
                profiles,
            } => {
                let k = landed_ending_range_index(crossings, current);
                landed.push(k);
                let crossing = &crossings[k];
                let remaining = current - crossing.crossing_gross_input;
                let ending = match &profiles[k] {
                    Some(profile) => profile.swap(remaining),
                    None => int_simulate_v3_swap(remaining, &crossing.ending_range),
                };
                let out = crossing.crossing_output.saturating_add(ending.output);
                hop_outputs.push(out);
                current = out;
            }
        }
    }

    WalkPathOutcome {
        final_output: hop_outputs.last().copied().unwrap_or(U256::ZERO),
        hop_outputs,
        landed,
    }
}

/// Build the per-hop shifted-piece inputs for tuple `ks`: each hop's
/// ending-range (or V2) state plus its crossing translations.
fn build_shifted_piece_hops(
    hops: &[WalkHop],
    ks: &[usize],
) -> Vec<crate::mobius_shifted_piece::ShiftedPieceHop> {
    use crate::mobius_shifted_piece::ShiftedPieceHop;
    hops.iter()
        .zip(ks.iter())
        .map(|(hop, &k)| match hop {
            WalkHop::ConstantProduct(hop_state) => ShiftedPieceHop {
                hop: (*hop_state).clone(),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
            WalkHop::Cl { crossings, .. } => {
                let crossing = &crossings[k];
                ShiftedPieceHop {
                    hop: crossing.ending_range.to_int_hop_state(),
                    gross_input_offset: if k > 0 {
                        crossing.crossing_gross_input
                    } else {
                        U256::ZERO
                    },
                    output_offset: crossing.crossing_output,
                }
            }
        })
        .collect()
}

/// Per-piece entry anchor : the exact affine-shifted Möbius
/// argmax of the piece's ending-range composition. Within one ending-range
/// piece the N-hop output is exactly Möbius (SL(2) closure of the per-hop CP
/// maps composed with the tick-crossing translations), so the argmax of
/// `P(x) = O(x) − x` is closed form:
/// `x* = (isqrt(A·D − B·C) − D)/C` — 0–2 wei from the window-refined discrete
/// argmax on interior-optimum pieces (see `mobius_shifted_piece`; the
/// unshifted+additive-gross formula is retained cfg(test)-only as the A/B
/// baseline).
///
/// Heuristic entry point only: the walk's correcting signal is the landed
/// tuple of the simulated candidate, not anchor precision, so correctness
/// never depends on the anchor. A piece whose optimum is a range-saturation
/// corner (the smooth argmax runs past the pinned edge) is owned by
/// `walk_refine_window`, which searches for the discrete peak.
#[cfg(test)]
pub(super) fn walk_piece_anchor(hops: &[WalkHop], ks: &[usize]) -> U256 {
    // Fresh single-shot reference for the doc tests: production uses the
    // memoizing `ShiftedPieceComposer` inline (byte-identical results).
    let pieces = build_shifted_piece_hops(hops, ks);
    let coeffs = crate::mobius_shifted_piece::compute_shifted_piece_mobius_coefficients(&pieces);
    crate::mobius_shifted_piece::shifted_piece_model_optimal_input(&coeffs).unwrap_or(U256::ZERO)
}

/// The transitional anchor (unshifted ending-range coefficients
/// plus additive `Σ crossing_gross_input`): misprices downstream crossings
/// (they are paid from an upstream hop's OUTPUT, not the path input).
/// Retained under cfg(test) as the A/B baseline for the exact-anchor
/// quality tests.
#[cfg(test)]
pub(super) fn walk_piece_anchor_transitional(hops: &[WalkHop], ks: &[usize]) -> U256 {
    let mut flat_hops: Vec<IntHopState> = Vec::with_capacity(hops.len());
    let mut gross_sum = U256::ZERO;
    for (hop, &k) in hops.iter().zip(ks.iter()) {
        match hop {
            WalkHop::ConstantProduct(hop_state) => flat_hops.push((*hop_state).clone()),
            WalkHop::Cl { crossings, .. } => {
                let crossing = &crossings[k];
                if k > 0 {
                    gross_sum = gross_sum.saturating_add(crossing.crossing_gross_input);
                }
                flat_hops.push(crossing.ending_range.to_int_hop_state());
            }
        }
    }
    let Ok(result) = crate::mobius_int_exact::exact_mobius_solve(&flat_hops) else {
        return gross_sum;
    };
    if !result.is_profitable || result.optimal_input.is_zero() {
        return gross_sum;
    }
    result.optimal_input.saturating_add(gross_sum)
}

/// Componentwise comparison helpers on landed tuples.
pub(super) fn landed_any_above(landed: &[usize], ks: &[usize]) -> bool {
    landed.iter().zip(ks.iter()).any(|(a, &b)| *a > b)
}

/// Profit score as a SIGNED value (`output − input`), so ternary refinement
/// can compare candidates on the unprofitable side without U256 underflow.
pub(super) fn walk_profit_score(output: U256, input: U256) -> alloy::primitives::I256 {
    use alloy::primitives::I256;
    let o = I256::try_from(output).unwrap_or(I256::MAX);
    let i = I256::try_from(input).unwrap_or(I256::MAX);
    o - i
}

/// Book-keeping for the best validated candidate seen by the walk.
pub(super) struct WalkRecorder {
    input: U256,
    profit: U256,
    hop_outputs: Vec<U256>,
    /// Best signed score across every evaluated candidate (including
    /// unprofitable ones) — the direction test's reference level.
    top_score: alloy::primitives::I256,
}

impl WalkRecorder {
    pub(super) fn new() -> Self {
        Self {
            input: U256::ZERO,
            profit: U256::ZERO,
            hop_outputs: Vec::new(),
            top_score: alloy::primitives::I256::MIN,
        }
    }

    /// Simulate `candidate`, update the bests, and return the outcome (the
    /// caller needs `landed` / `final_output` for direction decisions).
    fn eval_and_record(&mut self, candidate: U256, hops: &[WalkHop]) -> WalkPathOutcome {
        let outcome = simulate_walk_path(candidate, hops);
        let score = walk_profit_score(outcome.final_output, candidate);
        if score > self.top_score {
            self.top_score = score;
        }
        if outcome.final_output > candidate {
            let profit = outcome.final_output - candidate;
            if profit > self.profit {
                self.profit = profit;
                self.input = candidate;
                self.hop_outputs.clone_from(&outcome.hop_outputs);
            }
        }
        outcome
    }
}

fn anchor_sweep_mode(cfg: &SolveRuntimeConfig) -> AnchorSweep {
    cfg.anchor_sweep
}

/// Chain-saturation corner of a single-piece path (F1 guard), in PATH-INPUT
/// units: `hops[0]`'s range edge. The first hop's input equals the path input,
/// so its range edge is the sharp kink the unclamped smooth anchor can
/// overshoot — `P(x) = O(x) − x` turns down past it, so the peak can sit right
/// at the edge where an anchor-±2 probe already lands in the negative
/// post-cliff region. Only `hops[0]` is in path-input units: a later CL hop's
/// range edge is in ITS input units (the upstream output) and is deliberately
/// not compared to the path input here (such a kink is still bracketed by the
/// smooth anchor, see `single_piece_hop1_binding_kink_is_not_dropped`). The
/// single-piece analogue of the ≤4-wei `piece_window_right_edge` (which is
/// `None` when a hop has a single range). Returns `None` when the first hop
/// has no bounded range (constant product / unbounded), so callers fall back
/// to the anchor.
fn single_piece_saturation_edge(hops: &[WalkHop]) -> Option<U256> {
    let hop = hops.first()?;
    match hop {
        WalkHop::Cl { crossings, .. } => crossings
            .first()
            .map(|c| c.ending_range.max_gross_input_in_range()),
        WalkHop::ConstantProduct(_) => None,
    }
}

/// Maximum width the refine ternary settles to before the final probe grid.
/// `P(x)` per piece is concave with a shallow peak for liquid pools, so a
/// ~10⁶-wei bracket already contains a profit-optimal input; the caller pins
/// `hi` to the ≤4-wei right edge in the bounded (range-saturation) case, so
/// the grid always captures a corner max exactly. Measured to keep the
/// returned profit within ε of the exact-wei optimum (see the profit-ε gate
/// + the corner-profit test).
pub(super) const REFINE_BRACKET_WEI: u64 = 1_000_000;
/// Points probed across the final bracket (endpoints + interior) when the
/// bracket is wide. Concavity ⇒ the argmax sits in `[l, r]`; the grid (both
/// endpoints included) catches a flat interior top or an edge/corner max.
const REFINE_GRID_POINTS: u64 = 33;
/// Final-bracket width (wei) at/below which the refine sweeps to the wei
/// instead of using the coarse grid — narrow brackets (small ranges) may peak
/// sharply in the interior, so exactness there is worth the (≤1025) probes.
const REFINE_DENSE_SPAN: u64 = 1024;

/// Maximize profit over the piece window `[lo, hi]`: ternary to a coarse
/// bracket, then a probe grid over that bracket (or a wei-precise sweep for
/// narrow brackets).
///
/// Returns `(piece_argmax_x, piece_best_score)` — the location is informational
/// (candidates feed the shared [`WalkRecorder`], which owns the global argmax).
///
/// `P(x)` is concave (the EVM floor staircase perturbs it at wei scale
/// only), so ternary converges to the argmax neighborhood; the grid/sweep
/// picks the maximizer. This is a **profit-ε** search (not exact-wei): the
/// flat interior top makes the coarse grid profit-equivalent, and the bounded
/// corner is captured because `hi` is the pinned right edge.
pub(super) fn walk_refine_window(
    hops: &[WalkHop],
    lo: U256,
    hi: U256,
    rec: &mut WalkRecorder,
) -> (U256, alloy::primitives::I256) {
    use alloy::primitives::I256;
    let mut argmax_x = lo;
    let mut best_score = I256::MIN;
    // phase 0 = ternary narrowing, phase 1 = final grid / dense sweep.
    let mut probe = |x: U256, hops: &[WalkHop], rec: &mut WalkRecorder, phase: u8| -> I256 {
        WALK_REFINE_SIMS.with(|c| c.set(c.get() + 1));
        if phase == 0 {
            WALK_TERNARY_SIMS.with(|c| c.set(c.get() + 1));
        } else {
            WALK_GRID_SIMS.with(|c| c.set(c.get() + 1));
        }
        let o = rec.eval_and_record(x, hops);
        let s = walk_profit_score(o.final_output, x);
        if s > best_score {
            best_score = s;
            argmax_x = x;
        }
        s
    };
    let mut l = lo;
    let mut r = hi;
    while r.saturating_sub(l) > U256::from(REFINE_BRACKET_WEI) {
        let third = ((r - l) / U256::from(3u64)).max(U256::ONE);
        let m1 = l + third;
        let m2 = r - third;
        let s1 = probe(m1, hops, rec, 0);
        let s2 = probe(m2, hops, rec, 0);
        if s1 < s2 {
            l = m1 + U256::from(1u64);
        } else {
            r = m2.saturating_sub(U256::from(1u64));
        }
    }
    let span = r.saturating_sub(l);
    if span <= U256::from(REFINE_DENSE_SPAN) {
        // Narrow bracket → wei-precise sweep (cheap + exact for sharp peaks).
        let mut x = l;
        loop {
            probe(x, hops, rec, 1);
            if x >= r {
                break;
            }
            x += U256::from(1u64);
        }
    } else {
        // Wide bracket → coarse probe grid (endpoints + interior). Concavity ⇒
        // argmax in [l, r]; endpoints l (left) and r (the pinned right edge in
        // the bounded case) are both probed, so an interior flat top or a
        // range-saturation corner is both captured at profit-ε.
        let n = REFINE_GRID_POINTS;
        for i in 0..n {
            let x = l + (span * U256::from(i)) / U256::from(n - 1);
            probe(x, hops, rec, 1);
        }
    }
    (argmax_x, best_score)
}

/// One path's walk-combinator counters (D63GSE follow-up): the FULL set, so
/// solve telemetry can name the real cost driver — `sims × per-sim word_steps`
/// vs `refine_sims` (input-partition refinement probes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkStats {
    /// Tick-range pieces visited by the monotone walk.
    pub pieces: usize,
    /// Full path simulations (`simulate_walk_path` calls).
    pub sims: usize,
    /// `compute_swap_step_v3` word-boundary steps inside every sim — the
    /// per-simulation cost driver for dense (many-word) CL ranges.
    pub word_steps: usize,
    /// Stop-time refinement probes (`walk_refine_window` ternary + grid).
    pub refine_sims: usize,
    /// Ternary-narrowing phase sims (subset of `refine_sims`).
    pub ternary_sims: usize,
    /// Final grid / dense-sweep phase sims (subset of `refine_sims`).
    pub grid_sims: usize,
    /// Loop-13: left-window-edge bisection/scan probes.
    pub left_edge_sims: usize,
    /// Loop-13: right-window-edge (seeded) bisection probes.
    pub right_edge_sims: usize,
    /// Loop-13: transitional-anchor ±2 sweep probes.
    pub anchor_sims: usize,
    /// Event-solver pieces accepted on the verify probes (exact edge).
    pub event_solver_ok: usize,
    /// Event-solver pieces that fell back to grow + bisection.
    pub event_solver_fallbacks: usize,
    /// Largest word-boundary count any range reached (Q3 dense telemetry;
    /// the one-shot alert is the CONSUMER's decision — the walk reports).
    pub max_dense_words: usize,
    /// Loop-15 census tally (predicted vs bisected first-above). All-zero
    /// unless the census env gate is on.
    pub census: WalkEventCensus,
}

/// The walk entry's whole return: result + returned telemetry (SU7MAE
/// deepening — no frozen thread-locals on the read-back path; the entry
/// drains its own counters at entry and reports this stats value at exit).
#[derive(Debug, Default)]
pub struct WalkOutcome {
    /// `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
    pub result: Option<(U256, U256, Vec<U256>)>,
    /// This path's walk-combinator counters + census tally.
    pub stats: WalkStats,
    /// Census piece recorder (drained; empty unless the census gate is on).
    pub census_pieces: Vec<(Vec<usize>, U256)>,
}

impl WalkOutcome {
    pub(super) fn none() -> Self {
        Self {
            result: None,
            stats: WalkStats::default(),
            census_pieces: Vec::new(),
        }
    }

    pub(super) fn from_result(result: Option<(U256, U256, Vec<U256>)>) -> Self {
        Self {
            result,
            stats: WalkStats::default(),
            census_pieces: Vec::new(),
        }
    }
}
///    ([`piece_window_left_edge`] / [`piece_window_right_edge`]; consecutive
///    pieces share window edges, so the left edge reuses the previous
///    piece's right edge).
/// 3. Direction test: straddle probes at ±64 around the right edge with
///    +1-wei staircase tolerance. Climbing ⇒ advance ONE piece (never
///    hopscotch past unrefined pieces — concavity makes the refined
///    piece-maxima sequence unimodal, so consecutive visits cannot vault
///    the peak).
/// 4. Stop: refine the current piece AND its forward neighbor with a
///    windowed ternary + dense sweep ([`walk_refine_window`]) — refinement
///    is what makes the walk exact at range-saturation corners.
///
/// Termination is structural: each advance moves the tuple strictly forward
/// in the product order and landed tuples never retreat along an
/// x-increasing walk, so at most Σ ranges pieces are visited (+2 slack); a
/// visited set guards the pathological anchor-oscillation case.
/// Diagnostic telemetry thresholds for `solve_active_set_path`: a single solve
/// that exceeds either of these surfaces a `tracing::warn!` with per-hop range
/// counts so the operator can investigate the pool. Conservative — the
/// typical 3-hop solve visits tens of pieces; these thresholds only fire for
/// genuinely pathological pools (the walk visits every initialized tick in
/// the swap direction now that `max_ranges` is removed).
const SOLVE_TELEMETRY_PIECES_WARN: usize = 500;
const SOLVE_TELEMETRY_SIMS_WARN: usize = 50_000;

#[hotpath::measure(label = "cl_solve.active_set")]
pub(super) fn solve_active_set_path(hops: &[WalkHop], cfg: &SolveRuntimeConfig) -> WalkOutcome {
    let s_t0 = std::time::Instant::now();
    // The walk drains its own counters at entry; the returned outcome carries
    // THIS path's telemetry (no frozen thread-locals on the read-back path).
    reset_walk_stats();
    let out = solve_active_set_path_inner(hops, cfg);
    WALK_SOLVE_NS_TOTAL.fetch_add(
        u64::try_from(s_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    let stats = peek_walk_stats();
    let census_pieces = EVENT_CENSUS_PIECES.with_borrow_mut(std::mem::take);
    WalkOutcome {
        result: out,
        stats,
        census_pieces,
    }
}

#[expect(clippy::too_many_lines)]
fn solve_active_set_path_inner(
    hops: &[WalkHop],
    cfg: &SolveRuntimeConfig,
) -> Option<(U256, U256, Vec<U256>)> {
    /// Advance the landed tuple one piece past the window's right edge
    /// (the edge-bisection bracket is ≤4 wide, so scan a few steps).
    fn landed_beyond(hops: &[WalkHop], right_edge: U256, ks: &[usize]) -> Option<Vec<usize>> {
        for d in 1u64..=8 {
            let landed = simulate_walk_path(right_edge.saturating_add(U256::from(d)), hops).landed;
            if landed_any_above(&landed, ks) {
                return Some(landed);
            }
        }
        None
    }

    /// Stop-time refinement: ternary + dense sweep over this piece's window
    /// AND over its immediate forward neighbor's window. Refinement of
    /// climbed-through pieces is skipped during the walk (their interior
    /// maxima cannot beat the walk's terminal region under concavity); the
    /// neighbor refinement covers a peak straddling the edge that the ±1-wei
    /// staircase-tolerant direction test could mis-attribute.
    #[expect(
        clippy::too_many_arguments,
        reason = "walk-domain refinement carry (window pair + hint + recorder + neighbor switch + runtime stance) is coherent as a flat signature"
    )]
    fn refine_at_stop(
        hops: &[WalkHop],
        ks: &[usize],
        x_l: U256,
        x_r: Option<U256>,
        hint: U256,
        rec: &mut WalkRecorder,
        refine_neighbor: bool,
        cfg: &SolveRuntimeConfig,
    ) {
        let hi_current = x_r.unwrap_or_else(|| {
            hint.saturating_mul(U256::from(4u64))
                .max(x_l.saturating_mul(U256::from(2u64)))
                .max(x_l.saturating_add(U256::from(1024u64)))
        });
        if x_l <= hi_current {
            walk_refine_window(hops, x_l, hi_current, rec);
        }
        // Gated forward-neighbor refine (6V3ZS6 follow-up): a full ternary +
        // grid over the neighbor window runs on climbing stops (edge can
        // straddle a peak) and — on falling stops — only when a cheap coarse
        // 33-point evidence grid finds the neighbor competitive within a
        // 0.1% grace band of the walk's best score. The absolute skip was
        // measured WRONG by the fine-grid oracle (deep-liquidity family):
        // the ±64 straddle probe can land past a thin neighbor piece, so a
        // falling probe does NOT bound the neighbor's interior — the coarse
        // grid must adjudicate instead. Direct wei corner probes at the
        // shared edge (F1 per-piece analogue) run in every fall.
        if !refine_neighbor {
            let Some(xr) = x_r else {
                return;
            };
            let Some(next) = landed_beyond(hops, xr, ks) else {
                return;
            };
            let n_l = xr + U256::from(1u64);
            let n_r = piece_window_right_edge_evented(hops, &next, hint, None, None, cfg).0;
            let n_hi = n_r.unwrap_or_else(|| {
                hint.saturating_mul(U256::from(4u64))
                    .max(n_l.saturating_mul(U256::from(2u64)))
                    .max(n_l.saturating_add(U256::from(1024u64)))
            });
            if n_l <= n_hi {
                let span = n_hi - n_l;
                // Thin-edge peek trim: the +2-wei corner probe survives only
                // when the upcoming search does NOT cover it. The dense sweep
                // spans [n_l, n_hi] (covers +2 whenever span >= 2); the coarse
                // grid's first point is n_l (+1).
                if span < U256::from(2u64) {
                    rec.eval_and_record(xr + U256::from(2u64), hops);
                }
                let mut best_coarse = alloy::primitives::I256::MIN;
                for i in 0..33u64 {
                    let x = n_l + (span * U256::from(i)) / U256::from(32u64);
                    let o = rec.eval_and_record(x, hops);
                    let s = walk_profit_score(o.final_output, x);
                    if s > best_coarse {
                        best_coarse = s;
                    }
                }
                let grace = rec.top_score.max(alloy::primitives::I256::ZERO)
                    / alloy::primitives::I256::from_raw(alloy::primitives::U256::from(1_000u64));
                if best_coarse + grace >= rec.top_score {
                    walk_refine_window(hops, n_l, n_hi, rec);
                }
            }
            return;
        }
        let Some(xr) = x_r else {
            return;
        };
        let Some(next) = landed_beyond(hops, xr, ks) else {
            return;
        };
        let n_l = xr + U256::from(1u64);
        let n_r = piece_window_right_edge(hops, &next, hint);
        let n_hi = n_r.unwrap_or_else(|| {
            hint.saturating_mul(U256::from(4u64))
                .max(n_l.saturating_mul(U256::from(2u64)))
                .max(n_l.saturating_add(U256::from(1024u64)))
        });
        if n_l <= n_hi {
            walk_refine_window(hops, n_l, n_hi, rec);
        }
    }

    use alloy::primitives::I256;
    if hops.is_empty() {
        return None;
    }

    WALK_PIECES_VISITED.with(|c| c.set(0));
    WALK_PATH_SIMULATIONS.with(|c| c.set(0));

    let iteration_cap: usize = hops
        .iter()
        .map(|h| match h {
            WalkHop::ConstantProduct(_) => 1,
            WalkHop::Cl { crossings, .. } => crossings.len(),
        })
        .sum::<usize>()
        + 2;

    let mut ks = vec![0usize; hops.len()];
    let mut visited: HashSet<Vec<usize>> = HashSet::new();
    let mut rec = WalkRecorder::new();
    // Loop-17 anchor memoization: consecutive pieces share tuple prefixes,
    // so the Möbius fold reuses prefix partials (byte-exact — see
    // `ShiftedPieceComposer`). One composer per solve; `hops` is immutable
    // within a solve.
    let mut anchor_memo = crate::mobius_shifted_piece::ShiftedPieceComposer::new();
    // window boundaries, so it doubles as the next piece's left-edge scan
    // start (saves a full bisection per visited piece).
    let mut prev_right_edge: Option<U256> = None;
    // Bracket warm start for the right-edge bisection: the prior
    // piece's (edge, confirm_hi) pair. ks only advances componentwise, so
    // the prior edge remains a lower bound; the seeded helper re-validates
    // confirm_hi with a single probe. Byte-identical to the cold path.
    let mut right_bracket: Option<(U256, U256)> = None;

    let single_piece_path = hops.iter().all(|h| match h {
        WalkHop::ConstantProduct(_) => true,
        WalkHop::Cl { crossings, .. } => crossings.len() == 1,
    });

    for _ in 0..iteration_cap {
        if !visited.insert(ks.clone()) {
            break;
        }
        WALK_PIECES_VISITED.with(|c| c.set(c.get() + 1));

        // Transitional anchor: extra candidates (±2 sweep) and edge-growth
        // hint; never trusted for the direction decision.
        // Loop-17 A/B (EXPERIMENTAL, default ON): `DEGENBOT_WALK_ANCHOR_SWEEP=0`
        // disables the ±2 probe set to measure its value. The anchor VALUE is
        // still computed and used as the window-edge hint either way.
        let sweep = anchor_sweep_mode(cfg);
        let anchor_all_t0 = std::time::Instant::now();
        let anchor_build_t0 = std::time::Instant::now();
        let anchor_pieces = build_shifted_piece_hops(hops, &ks);
        WALK_ANCHOR_BUILD_NS.fetch_add(
            u64::try_from(anchor_build_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        let anchor_compose_t0 = std::time::Instant::now();
        let anchor_coeffs = anchor_memo.piece_coefficients(&anchor_pieces, &ks);
        WALK_ANCHOR_COMPOSE_NS.fetch_add(
            u64::try_from(anchor_compose_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        let anchor_argmax_t0 = std::time::Instant::now();
        let anchor = crate::mobius_shifted_piece::shifted_piece_model_optimal_input(&anchor_coeffs)
            .unwrap_or(U256::ZERO);
        WALK_ANCHOR_ARGMAX_NS.fetch_add(
            u64::try_from(anchor_argmax_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        WALK_ANCHOR_NS_TOTAL.fetch_add(
            u64::try_from(anchor_all_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        if !anchor.is_zero() {
            let anchor_t0 = WALK_PATH_SIMULATIONS.with(std::cell::Cell::get);
            let deltas: &[i32] = match sweep {
                AnchorSweep::Full => &[-2, -1, 0, 1, 2],
                AnchorSweep::CenterOnly => &[0],
                AnchorSweep::Off => &[],
            };
            for &delta in deltas {
                let candidate = match delta.cmp(&0) {
                    std::cmp::Ordering::Equal => anchor,
                    std::cmp::Ordering::Greater => {
                        anchor.saturating_add(U256::from(delta.unsigned_abs()))
                    }
                    std::cmp::Ordering::Less => {
                        anchor.saturating_sub(U256::from(delta.unsigned_abs()))
                    }
                };
                if candidate.is_zero() {
                    continue;
                }
                rec.eval_and_record(candidate, hops);
            }
            let anchor_delta = WALK_PATH_SIMULATIONS.with(std::cell::Cell::get) - anchor_t0;
            WALK_ANCHOR_SIMS.with(|c| c.set(c.get() + anchor_delta));
        }
        if single_piece_path {
            // F1 corner guard (adversarial review): the exact unclamped smooth
            // anchor can overshoot this piece's saturation corner (the
            // chain-saturation input), where a sharp kink holds the true max —
            // the `anchor ± 2` probe above is then in the negative post-cliff
            // region and records nothing. Refine the terminal window (lo=0)
            // with hi floored at the piece's saturation edge, so the corner is
            // always bracketed; interior single-piece peaks (anchor inside the
            // range) are a strict superset search and stay correct. Bounded
            // (multi-piece) paths are untouched.
            let sat = single_piece_saturation_edge(hops);
            // The saturation corner (the kink) is a flat-plateau peak a
            // ternary/grid refine can land short of, and it is the true max
            // exactly when the anchor overshoots it — so probe it (and the wei
            // just below, where the peak may sit) directly. Covers anchor =
            // 0/MAX (the corner is the floor). Pure-CP (no corner) → skip.
            if let Some(e) = sat {
                if e > U256::ZERO {
                    rec.eval_and_record(e, hops);
                    rec.eval_and_record(e - U256::from(1), hops);
                }
            }
            let hi = sat.map_or(anchor.max(U256::from(1024)), |e| e.max(anchor));
            if hi > U256::ZERO {
                let rq_mk = Mark::start();
                walk_refine_window(hops, U256::ZERO, hi, &mut rec);
                rq_mk.commit(
                    &WALK_CENSUS_REFINE_NS,
                    &WALK_CENSUS_REFINE_SIMS,
                    &WALK_CENSUS_REFINE_SIMNS,
                );
            }
            break;
        }

        // Window left edge: reuse the previous piece's right edge when
        // walking consecutively (scan a few steps forward); fall back to a
        // full bisection otherwise. (Section-census timers on every exit.)
        let le_mk = Mark::start();
        let x_l = if ks.iter().all(|&k| k == 0) {
            U256::ZERO
        } else if let Some(prev) = prev_right_edge {
            let mut found = None;
            for d in 1u64..=9 {
                let probe = prev + U256::from(d);
                let landed = simulate_walk_path(probe, hops).landed;
                if !landed.iter().zip(ks.iter()).any(|(a, &b)| *a < b) {
                    found = Some(probe);
                    break;
                }
            }
            match found {
                Some(x) => x,
                None => piece_window_left_edge(hops, &ks, anchor),
            }
        } else {
            piece_window_left_edge(hops, &ks, anchor)
        };

        // Skipped tuple: `x_l` lands strictly ABOVE `ks` in some component —
        // the lattice path never lands exactly on `ks`. Advance without
        // treating this as a real piece.
        {
            let landed = simulate_walk_path(x_l, hops).landed;
            if landed != ks {
                if landed_any_above(&landed, &ks) {
                    ks = landed;
                    prev_right_edge = None;
                    le_mk.commit(
                        &WALK_CENSUS_EDGE_NS,
                        &WALK_CENSUS_EDGE_SIMS,
                        &WALK_CENSUS_EDGE_SIMNS,
                    );
                    continue;
                }
                // landed BELOW ks means the left-edge search went wrong;
                // fall back to a full edge computation before giving up.
                let x_l_full = piece_window_left_edge(hops, &ks, anchor);
                let landed_full = simulate_walk_path(x_l_full, hops).landed;
                if landed_full != ks {
                    if landed_any_above(&landed_full, &ks) {
                        ks = landed_full;
                        prev_right_edge = None;
                        le_mk.commit(
                            &WALK_CENSUS_EDGE_NS,
                            &WALK_CENSUS_EDGE_SIMS,
                            &WALK_CENSUS_EDGE_SIMNS,
                        );
                        continue;
                    }
                    le_mk.commit(
                        &WALK_CENSUS_EDGE_NS,
                        &WALK_CENSUS_EDGE_SIMS,
                        &WALK_CENSUS_EDGE_SIMNS,
                    );
                    break; // degenerate piece — terminate with what we have
                }
            }
        }
        le_mk.commit(
            &WALK_CENSUS_EDGE_NS,
            &WALK_CENSUS_EDGE_SIMS,
            &WALK_CENSUS_EDGE_SIMNS,
        );

        let re_mk = Mark::start();
        let (x_r, right_confirm_hi) = piece_window_right_edge_evented(
            hops,
            &ks,
            anchor,
            right_bracket.map(|b| b.0),
            right_bracket.map(|b| b.1),
            cfg,
        );
        // Loop-15 census (`DEGENBOT_WALK_EVENT_CENSUS=1`): the nested
        // ceil-inversion's prediction vs this bisection bracket. No-op
        // (one bool load) when the gate is unset.
        event_census_record(hops, &ks, x_r, right_confirm_hi, cfg);
        re_mk.commit(
            &WALK_CENSUS_REDGE_NS,
            &WALK_CENSUS_REDGE_SIMS,
            &WALK_CENSUS_REDGE_SIMNS,
        );
        let Some(xr) = x_r else {
            // Terminal piece (unbounded right): refine and finish.
            let term_mk = Mark::start();
            refine_at_stop(hops, &ks, x_l, None, anchor, &mut rec, false, cfg);
            term_mk.commit(
                &WALK_CENSUS_REFINE_NS,
                &WALK_CENSUS_REFINE_SIMS,
                &WALK_CENSUS_REFINE_SIMNS,
            );
            break;
        };
        prev_right_edge = Some(xr);
        right_bracket = Some((xr, right_confirm_hi));

        // Loop-17 anchor memoization: consecutive pieces share tuple prefixes,
        // so the Möbius fold reuses prefix partials (byte-exact — see
        // `ShiftedPieceComposer`). One composer per solve; `hops` is immutable
        // within a solve.
        // Direction test: straddle probes at ±64 around the window’s right
        // edge, with +1-wei staircase tolerance. Climbing ⇒ advance one
        // piece; falling or level ⇒ the peak is at or behind this edge —
        // stop and refine this piece plus its forward neighbor.
        let di_mk = Mark::start();
        let back = xr.saturating_sub(U256::from(64u64)).max(x_l);
        let fwd = xr.saturating_add(U256::from(64u64));
        let score_back = walk_profit_score(rec.eval_and_record(back, hops).final_output, back);
        let score_fwd = walk_profit_score(rec.eval_and_record(fwd, hops).final_output, fwd);
        let climbing = score_fwd + I256::ONE >= score_back;
        let advance = if climbing {
            landed_beyond(hops, xr, &ks)
        } else {
            None
        };
        di_mk.commit(
            &WALK_CENSUS_DIR_NS,
            &WALK_CENSUS_DIR_SIMS,
            &WALK_CENSUS_DIR_SIMNS,
        );
        if let Some(next) = advance {
            ks = next;
            continue;
        }
        let term_mk = Mark::start();
        refine_at_stop(hops, &ks, x_l, Some(xr), anchor, &mut rec, climbing, cfg);
        term_mk.commit(
            &WALK_CENSUS_REFINE_NS,
            &WALK_CENSUS_REFINE_SIMS,
            &WALK_CENSUS_REFINE_SIMNS,
        );
        break;
    }

    // Post-hoc telemetry: the walk now visits every initialized tick in
    // the swap direction (no `max_ranges` cap). The solver's own bounds
    // (`iteration_cap`, `prune`, `REFINE_GRID_POINTS`) prevent runaway cost,
    // but a pathological pool can still burn excessive pieces or
    // simulations. Surface those cases for diagnosis (not for screening —
    // the solve still completes and returns its result).
    let ws = peek_walk_stats();
    let over_threshold =
        ws.pieces > SOLVE_TELEMETRY_PIECES_WARN || ws.sims > SOLVE_TELEMETRY_SIMS_WARN;
    if over_threshold {
        let n_hops = hops.len();
        let mut range_counts = Vec::with_capacity(n_hops);
        let mut total_ranges = 0usize;
        for h in hops {
            let n = match h {
                WalkHop::ConstantProduct(_) => 1,
                WalkHop::Cl { crossings, .. } => crossings.len(),
            };
            range_counts.push(n);
            total_ranges += n;
        }
        op_warn!(domain = solver, pieces = ws.pieces,
            sims = ws.sims,
            word_steps = ws.word_steps,
            refine_sims = ws.refine_sims,
            n_hops,
            total_ranges,
            range_counts = ?range_counts,
            "active-set solve burned excessive solver time (no solve-side cap; surface for pool diagnosis)"
        );
    }

    if rec.profit.is_zero() {
        None
    } else {
        Some((rec.input, rec.profit, rec.hop_outputs))
    }
}
