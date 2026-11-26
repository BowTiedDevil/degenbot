use std::sync::Arc;

use alloy::primitives::U256;

use degenbot_core::op_warn;
use degenbot_math::v2::IntHopState;

use super::crossings::{
    concentrated_liquidity_hop_min_input_for_output, landed_ending_range_index,
    piece_window_left_edge, piece_window_right_edge, piece_window_right_edge_evented,
};
use super::cursor::{TerminalKind, WalkCursor, WalkTransition};
pub(super) use super::direction::walk_profit_score;
use super::direction::{direction_at_edge, DirectionAction, DirectionObservation};
use super::evaluation::{ProbeKind, WalkEvaluator};
use super::hop_sim::{bounded_tail, simulate_v3_range_swap};
#[cfg(test)]
pub(super) use super::refine::REFINE_BRACKET_WEI;
pub(super) use super::refine::{RefineWindow, Refiner, REFINE_DENSE_SPAN};
use super::tally::{WalkSection, WalkTally, WalkTelemetry};
use super::telemetry::{event_census_record, Mark, WalkEventCensus};
use super::window::RefineBound;
use super::{
    ConcentratedLiquidityCrossingTable, ConcentratedLiquidityProfileTable, IntV3TickRangeSequence,
};
use crate::solve::runtime::{AnchorSweep, SolveRuntimeConfig};
use crate::state::int_v3_hop::SwapWalkCensus;

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

/// A hop in the active-set walk, projected at construction into exactly the
/// capabilities the walk consumes. The walk never branches on the family:
/// the per-family simulators sit behind this seam, so this struct is the one
/// place a constant-product hop and a concentrated-liquidity hop differ.
pub(super) struct PieceView<'a> {
    family: PieceFamily<'a>,
    /// The tick-range sequence this hop was derived from, when it has one: the
    /// only source of the range exit kinds and interior word boundaries the
    /// swap-loop census reads ([`IntV3TickRangeSequence::swap_census`]).
    /// `None` for constant product — a hop with no tick map has no census.
    seq: Option<&'a IntV3TickRangeSequence>,
    /// Walk pieces this hop contributes: one per ending range for concentrated liquidity, one
    /// for constant product.
    piece_count: usize,
    /// True when the hop is a single walk piece; the walk skips the
    /// multi-piece lattice machinery then.
    single_piece: bool,
    /// First range's gross-input saturation edge in path-input units, when
    /// the first range is bounded; `None` for constant product.
    bounded_first_range: Option<U256>,
}

/// The per-family payload behind [`PieceView`], matched only inside the
/// view's methods - never in a per-candidate walk loop.
enum PieceFamily<'a> {
    /// V2-family constant-product hop. The landed tuple entry is always 0.
    ConstantProduct(&'a IntHopState),
    /// concentrated liquidity hop with its pre-computed per-index crossing table (`crossings[k]`
    /// is [`IntV3TickRangeSequence::compute_crossing`]`(k)`).
    ConcentratedLiquidity {
        /// Crossing data for every ending-range index `k` in `0..ranges.len()`.
        /// `Arc`-backed so the projection's precomputed table is shared - not
        /// re-cloned - across every path reusing the hop.
        crossings: Arc<ConcentratedLiquidityCrossingTable>,
        /// Optional precomputed forward word-boundary profile per `crossings[k]`
        /// (dense ranges only; `None` keeps those on the linear walk). Parallel to
        /// `crossings`. `Arc`-backed so the projection's precomputed profile (on
        /// the [`crate::solve::ResolvedHop`]) is shared - not re-cloned - across
        /// every path reusing the hop (the hop-projection memoization).
        profiles: Arc<ConcentratedLiquidityProfileTable>,
    },
}

impl<'a> PieceView<'a> {
    pub(super) fn constant_product(hop: &'a IntHopState) -> Self {
        Self {
            family: PieceFamily::ConstantProduct(hop),
            seq: None,
            piece_count: 1,
            single_piece: true,
            bounded_first_range: None,
        }
    }

    pub(super) fn concentrated_liquidity(
        seq: &'a IntV3TickRangeSequence,
        crossings: Arc<ConcentratedLiquidityCrossingTable>,
        profiles: Arc<ConcentratedLiquidityProfileTable>,
    ) -> Self {
        let piece_count = crossings.len();
        let bounded_first_range = crossings
            .first()
            .map(|c| c.ending_range.max_gross_input_in_range());
        Self {
            family: PieceFamily::ConcentratedLiquidity {
                crossings,
                profiles,
            },
            seq: Some(seq),
            piece_count,
            single_piece: piece_count == 1,
            bounded_first_range,
        }
    }

    pub(super) fn piece_count(&self) -> usize {
        self.piece_count
    }

    pub(super) fn single_piece(&self) -> bool {
        self.single_piece
    }

    pub(super) fn bounded_first_range(&self) -> Option<U256> {
        self.bounded_first_range
    }

    /// Forward step for a non-zero `current`: the hop output and the swap
    /// loop's facts for this evaluation (the landed ending-range index and the
    /// landing fill's word steps).
    pub(super) fn step(&self, current: U256, tally: &mut WalkTally) -> (U256, StepFacts) {
        match &self.family {
            PieceFamily::ConstantProduct(hop_state) => {
                // V2 overflow-reverts on-chain → the path yields nothing there.
                let out = hop_state.swap(current).unwrap_or(U256::ZERO);
                (out, StepFacts::default())
            }
            PieceFamily::ConcentratedLiquidity {
                crossings,
                profiles,
            } => {
                let k = landed_ending_range_index(crossings, current);
                let crossing = &crossings[k];
                let remaining = current - crossing.crossing_gross_input;
                let ending = match &profiles[k] {
                    Some(profile) => profile.swap(remaining, tally),
                    None => simulate_v3_range_swap(remaining, &crossing.ending_range, tally),
                };
                (
                    crossing.crossing_output.saturating_add(ending.output),
                    StepFacts {
                        landed: k,
                        word_steps: ending.word_steps,
                        // The fill placed less than it was handed only when the
                        // sequence's modeled span ran out first: the swap goes
                        // further than the walk sees, so its census counts are
                        // lower bounds.
                        bounded_tail: bounded_tail(remaining, ending.consumed_input),
                    },
                )
            }
        }
    }

    /// The swap LOOP's census for one evaluation of this hop, from the step's
    /// own facts — the gas floor's per-hop input (`docs/gas-estimation.md`).
    ///
    /// `None` when the hop carries no sequence to read (constant product, or a
    /// hand-built sequence with no tick map).
    pub(super) fn census(&self, facts: StepFacts) -> Option<SwapWalkCensus> {
        self.seq
            .and_then(|seq| seq.swap_census(facts.landed, facts.word_steps, facts.bounded_tail))
    }

    /// The anchor-time projection of piece `k`: the landed range (or
    /// constant-product) state plus its crossing translations.
    pub(super) fn anchor_piece(
        &self,
        k: usize,
    ) -> crate::solve::mobius_shifted_piece::ShiftedPieceHop {
        use crate::solve::mobius_shifted_piece::ShiftedPieceHop;
        match &self.family {
            PieceFamily::ConstantProduct(hop_state) => ShiftedPieceHop {
                hop: (*hop_state).clone(),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
            PieceFamily::ConcentratedLiquidity { crossings, .. } => {
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
        }
    }

    /// Gross input of the crossing boundary after landed range `k`, when the
    /// hop has one: `None` for constant product or a terminal range.
    pub(super) fn next_boundary_gross(&self, k: usize) -> Option<U256> {
        match &self.family {
            PieceFamily::ConstantProduct(_) => None,
            PieceFamily::ConcentratedLiquidity { crossings, .. } => {
                crossings.get(k + 1).map(|c| c.crossing_gross_input)
            }
        }
    }

    /// Smallest input into this hop that lands in range `k` and yields at
    /// least `demand`; `None` when the demand is unreachable within the
    /// landing (constant-product overflow-revert, concentrated liquidity capacity, or an
    /// upstream exit preempting the next boundary).
    pub(super) fn min_input_for_output(
        &self,
        k: usize,
        demand: U256,
        tally: &mut WalkTally,
    ) -> Option<U256> {
        match &self.family {
            PieceFamily::ConstantProduct(hop_state) => hop_state.swap_exact_out(demand).ok(),
            PieceFamily::ConcentratedLiquidity {
                crossings,
                profiles,
            } => {
                let crossing = &crossings[k];
                let z = concentrated_liquidity_hop_min_input_for_output(
                    crossing,
                    profiles[k].as_deref(),
                    demand,
                    tally,
                )?;
                if let Some(next_boundary) = crossings.get(k + 1) {
                    if z >= next_boundary.crossing_gross_input {
                        return None;
                    }
                }
                Some(z)
            }
        }
    }
}

/// The swap loop's per-hop facts one forward step produces beyond its output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct StepFacts {
    /// Ending-range index the input landed in (always 0 for a V2 hop).
    pub(super) landed: usize,
    /// Bitmap-word boundaries the landing fill completed.
    pub(super) word_steps: usize,
    /// The fill ran out of MODELLED range with input left over (the census
    /// counts are lower bounds).
    pub(super) bounded_tail: bool,
}

/// Result of a [`simulate_walk_path`] evaluation: the per-hop outputs, the
/// ending-range tuple the input actually landed in, and the per-hop step facts
/// the census is projected from.
#[derive(Debug, Default)]
pub(super) struct WalkPathOutcome {
    /// Output after the last hop.
    pub(super) final_output: U256,
    /// `hop_outputs[i]` = output after hop `i`.
    pub(super) hop_outputs: Vec<U256>,
    /// Ending-range index landed in per hop (always 0 for V2 hops).
    pub(super) landed: Vec<usize>,
    /// `facts[i]` = hop `i`'s step facts (the census source).
    pub(super) facts: Vec<StepFacts>,
}

impl WalkPathOutcome {
    pub(super) fn model_domain_exhausted(&self) -> bool {
        !self.facts.is_empty() && self.facts.iter().all(|facts| facts.bounded_tail)
    }
}

/// Simulate the path with SELF-DETERMINED crossings: each concentrated liquidity hop's ending
/// range is derived from the gross input actually available at that hop
/// (hop 0: `amount_in`; hop i: hop `i−1`'s output).
///
/// Unlike `int_simulate_concentrated_liquidity_path_n` (which simulates under an ASSUMED crossing
/// tuple and returns the zero-exhaustion shape when the input cannot afford
/// the assumption), this walker always simulates the piece the input truly
/// lands in — which is what makes it usable as the walk's ground truth for
/// any candidate.
#[cfg_attr(
    not(any(test, feature = "telemetry")),
    expect(
        dead_code,
        reason = "owned-result wrapper is used by tests and telemetry"
    )
)]
pub(super) fn simulate_walk_path(
    amount_in: U256,
    hops: &[PieceView],
    tally: &mut WalkTally,
) -> WalkPathOutcome {
    let mut outcome = WalkPathOutcome::default();
    simulate_walk_path_into(amount_in, hops, tally, &mut outcome);
    outcome
}

/// Evaluate into caller-owned buffers, retaining their capacity between
/// probes. The allocating [`simulate_walk_path`] wrapper remains useful when
/// an owned result is needed; hot evaluation loops reuse one outcome instead.
pub(super) fn simulate_walk_path_into(
    amount_in: U256,
    hops: &[PieceView],
    tally: &mut WalkTally,
    outcome: &mut WalkPathOutcome,
) {
    tally.bump_sims(1);
    let sim_t0 = std::time::Instant::now();
    simulate_walk_path_inner(amount_in, hops, tally, outcome);
    tally.add_sim_ns(u64::try_from(sim_t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
}

fn simulate_walk_path_inner(
    amount_in: U256,
    hops: &[PieceView],
    tally: &mut WalkTally,
    outcome: &mut WalkPathOutcome,
) {
    let n_hops = hops.len();
    outcome.final_output = U256::ZERO;
    outcome.hop_outputs.clear();
    outcome.landed.clear();
    outcome.facts.clear();
    outcome.hop_outputs.reserve(n_hops);
    outcome.landed.reserve(n_hops);
    outcome.facts.reserve(n_hops);
    let mut current = amount_in;

    for hop in hops {
        if current.is_zero() {
            outcome.hop_outputs.push(U256::ZERO);
            outcome.landed.push(0);
            outcome.facts.push(StepFacts::default());
            continue;
        }
        let (out, step) = hop.step(current, tally);
        outcome.landed.push(step.landed);
        outcome.facts.push(step);
        outcome.hop_outputs.push(out);
        current = out;
    }

    outcome.final_output = outcome.hop_outputs.last().copied().unwrap_or(U256::ZERO);
}

/// Build the per-hop shifted-piece inputs for tuple `ks`: each hop's
/// ending-range (or V2) state plus its crossing translations.
fn build_shifted_piece_hops(
    hops: &[PieceView],
    ks: &[usize],
) -> Vec<crate::solve::mobius_shifted_piece::ShiftedPieceHop> {
    hops.iter()
        .zip(ks.iter())
        .map(|(hop, &k)| hop.anchor_piece(k))
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
/// `Refiner::refine_window`, which searches for the discrete peak.
#[cfg(test)]
pub(super) fn walk_piece_anchor(hops: &[PieceView], ks: &[usize]) -> U256 {
    // Fresh single-shot reference for the doc tests: production uses the
    // memoizing `ShiftedPieceComposer` inline (byte-identical results).
    let pieces = build_shifted_piece_hops(hops, ks);
    let coeffs =
        crate::solve::mobius_shifted_piece::compute_shifted_piece_mobius_coefficients(&pieces);
    crate::solve::mobius_shifted_piece::shifted_piece_model_optimal_input(&coeffs)
        .unwrap_or(U256::ZERO)
}

/// The transitional anchor (unshifted ending-range coefficients
/// plus additive `Σ crossing_gross_input`): misprices downstream crossings
/// (they are paid from an upstream hop's OUTPUT, not the path input).
/// Retained under cfg(test) as the A/B baseline for the exact-anchor
/// quality tests.
#[cfg(test)]
pub(super) fn walk_piece_anchor_transitional(hops: &[PieceView], ks: &[usize]) -> U256 {
    let mut flat_hops: Vec<IntHopState> = Vec::with_capacity(hops.len());
    let mut gross_sum = U256::ZERO;
    for (hop, &k) in hops.iter().zip(ks.iter()) {
        let piece = hop.anchor_piece(k);
        gross_sum = gross_sum.saturating_add(piece.gross_input_offset);
        flat_hops.push(piece.hop);
    }
    let Ok(result) = crate::solve::mobius_int_anchor::anchored_mobius_solve(&flat_hops) else {
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

fn anchor_sweep_mode(cfg: &SolveRuntimeConfig) -> AnchorSweep {
    cfg.anchor_sweep
}

/// Chain-saturation corner of a single-piece path (F1 guard), in PATH-INPUT
/// units: `hops[0]`'s range edge. The first hop's input equals the path input,
/// so its range edge is the sharp kink the unclamped smooth anchor can
/// overshoot — `P(x) = O(x) − x` turns down past it, so the peak can sit right
/// at the edge where an anchor-±2 probe already lands in the negative
/// post-cliff region. Only `hops[0]` is in path-input units: a later concentrated liquidity hop's
/// range edge is in ITS input units (the upstream output) and is deliberately
/// not compared to the path input here (such a kink is still bracketed by the
/// smooth anchor, see `single_piece_hop1_binding_kink_is_not_dropped`). The
/// single-piece analogue of the ≤4-wei `piece_window_right_edge` (which is
/// `None` when a hop has a single range). Returns `None` when the first hop
/// has no bounded range (constant product / unbounded), so callers fall back
/// to the anchor.
fn single_piece_saturation_edge(hops: &[PieceView]) -> Option<U256> {
    hops.first()?.bounded_first_range()
}

/// Compile-time stance for anchor-overshoot re-entry. A
/// `SolveRuntimeConfig` field would require editing the exhaustive struct
/// literal in `degenbot-bot`'s engine lifecycle, outside this crate. The extra
/// refine runs only on deep-book paths whose fee-dominated entry hides the peak.
const REFINE_ADAPTIVE_ANCHOR_OVERSHOOT: bool = true;
/// Minimum walk-piece count (`Σ range counts + 2`) below which re-entry
/// stays dormant. Re-entry targets deep books whose fee-dominated leading
/// ranges can hide the peak; on small books it only shuffles equal-profit
/// plateau points.
const REFINE_ADAPTIVE_MIN_TOTAL_PIECES: usize = 64;

/// One path's walk-combinator counters: the FULL set, so
/// solve telemetry can name the real cost driver — `sims × per-sim word_steps`
/// vs `refine_sims` (input-partition refinement probes).
///
/// # Counter policy: every field is feature-gated — no field is always real
///
/// All twelve `usize` counters (`pieces`, `sims`, `word_steps`, `refine_sims`,
/// `ternary_sims`, `grid_sims`, `left_edge_sims`, `right_edge_sims`,
/// `anchor_sims`, `event_solver_ok`, `event_solver_fallbacks`,
/// `max_dense_words`) and `census` are produced behind the crate's
/// `telemetry` feature (default OFF; see `Cargo.toml`). Without it every
/// writer is compiled to a no-op (the per-field gates in `concentrated_liquidity/tally.rs`) and
/// the tally projects `WalkStats::default()`, so every field reads 0 in
/// default builds.
/// With the feature on, the `usize` counters are live; `census` additionally
/// needs the runtime census gate (`SolveRuntimeConfig::walk_event_census`) —
/// feature on + gate off leaves `census` dark while the `usize` counters
/// still count.
///
/// The cheap counters share the heavy census/timing machinery's gate
/// deliberately — one counter policy, not an accident of colocated code: the
/// bumps ride the walk's innermost loops (one per path simulation, per
/// word-boundary step, per refinement probe, per piece), and the crate's
/// zero-overhead default (module doc of `concentrated_liquidity/telemetry.rs`: the release build
/// "pays nothing" for telemetry) keeps every write out of production builds.
/// Default-build consumers must therefore read zeros as "unmeasured", never
/// as "the walk did nothing" — use `WalkOutcome::result` for that — and take
/// cost/progress facts from feature-independent instruments (`WalkMemo`
/// probe stats: `cache_plays`, `negatives_played`; the walk `result` itself).
/// The bounded-counts test (`concentrated_liquidity/tests/mod.rs`) enforces the zero flush; the
/// memo tests cfg-split their `stats.sims` assertions the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkStats {
    /// Tick-range pieces visited by the monotone walk.
    pub pieces: usize,
    /// Full path simulations (`simulate_walk_path` calls).
    pub sims: usize,
    /// `compute_swap_step_v3` word-boundary steps inside every sim — the
    /// per-simulation cost driver for dense (many-word) concentrated liquidity ranges.
    pub word_steps: usize,
    /// Stop-time refinement probes (`Refiner::refine_window` ternary + grid).
    pub refine_sims: usize,
    /// Ternary-narrowing phase sims (subset of `refine_sims`).
    pub ternary_sims: usize,
    /// Final grid / dense-sweep phase sims (subset of `refine_sims`).
    pub grid_sims: usize,
    /// Left-window-edge bisection/scan probes.
    pub left_edge_sims: usize,
    /// Right-window-edge (seeded) bisection probes.
    pub right_edge_sims: usize,
    /// Anchor-sweep probes.
    pub anchor_sims: usize,
    /// Event-solver pieces accepted on the verify probes (exact edge).
    pub event_solver_ok: usize,
    /// Event-solver pieces that fell back to grow + bisection.
    pub event_solver_fallbacks: usize,
    /// Largest word-boundary count any range reached (dense-book telemetry;
    /// the one-shot alert is the CONSUMER's decision — the walk reports).
    pub max_dense_words: usize,
    /// Event census tally (predicted vs bisected first-above). All-zero
    /// unless the runtime census gate is on (`SolveRuntimeConfig::
    /// walk_event_census`) — and, like every field here, needs the
    /// `telemetry` feature first (see the struct-level counter policy).
    pub census: WalkEventCensus,
}

/// Why a solve surface declined WITHOUT walking (closed vocabulary; each
/// variant names the thing it fell from). A walk that runs and finds nothing
/// profitable is NOT a refusal — it carries `refusal: None` — so a caller can
/// tell "this shape is not mine" from "this composition has no profit". Two
/// other no-value facts deliberately ride outside this vocabulary: the
/// profit-envelope gate's rejection (`SolveOutcome::envelope`) and the V4
/// int128 hard reject (that walk RAN; its stats show it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkRefusal {
    /// Fewer than two hops: not a walkable composition.
    TooShort,
    /// A hop family no solve surface here consumes; the caller routes it.
    NotWalkable,
    /// A partially-resolved path (hops missing) — never solved.
    Unresolved,
}

/// The walk's winning candidate: the solved amounts plus the per-hop swap-loop
/// census that THIS evaluation produced.
///
/// The census rides the value because it is a fact about the evaluation, not a
/// property of the composition: the memo stores it with the value, so a hit
/// replays the gas input too and admission never re-walks a path to price it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkSolution {
    /// The path input the walk's best candidate consumed.
    pub optimal_input: U256,
    /// `final_output − optimal_input` at that candidate.
    pub profit: U256,
    /// `hop_outputs[i]` = output after hop `i`.
    pub hop_outputs: Vec<U256>,
    /// `census[i]` = hop `i`'s swap-loop census at `optimal_input`; `None` for
    /// a constant-product hop or a sequence with no tick map.
    pub census: Vec<Option<SwapWalkCensus>>,
}

/// The walk entry's whole return: result + typed refusal + returned telemetry
/// (no frozen thread-locals on the read-back path; the entry drains its own
/// counters at entry and reports this stats value at exit).
#[derive(Debug, Default)]
pub struct WalkOutcome {
    /// The walked value (amounts + gas census) or `None` if not profitable.
    pub result: Option<WalkSolution>,
    /// Set only when the surface declined without walking (see
    /// [`WalkRefusal`]); `None` on every walked outcome.
    pub refusal: Option<WalkRefusal>,
    /// This path's walk-combinator counters + census tally.
    pub stats: WalkStats,
    /// Census piece recorder (drained; empty unless the census gate is on).
    pub census_pieces: Vec<(Vec<usize>, U256)>,
}

impl WalkOutcome {
    /// A walked outcome with no profitable value (the memo's cached-negative
    /// replay lands here too — the walk once ran and found nothing).
    pub(super) fn none() -> Self {
        Self {
            result: None,
            refusal: None,
            stats: WalkStats::default(),
            census_pieces: Vec::new(),
        }
    }

    /// The surface declined before walking: no value, no stats, the named
    /// refusal.
    pub(super) fn refused(refusal: WalkRefusal) -> Self {
        Self {
            result: None,
            refusal: Some(refusal),
            stats: WalkStats::default(),
            census_pieces: Vec::new(),
        }
    }

    pub(super) fn from_result(result: Option<WalkSolution>) -> Self {
        Self {
            result,
            refusal: None,
            stats: WalkStats::default(),
            census_pieces: Vec::new(),
        }
    }
}
/// Active-set path search:
/// 1. Build the current piece's shifted Möbius anchor.
/// 2. Compute the piece window with [`piece_window_left_edge`] and
///    [`piece_window_right_edge`]; consecutive pieces share window edges, so
///    the left edge reuses the previous piece's right edge.
/// 3. Compare a fixed rear probe at `max(x_r - 64, x_l)` with forward probes
///    at `x_r + span`, starting at 64 and doubling. Advance one piece when
///    `score_fwd >= score_back`; equality advances. Otherwise stop only when
///    every hop reports `bounded_tail` or the forward input reaches
///    `U256::MAX`. A hop without bounded-tail evidence, including constant
///    product, leaves input saturation as the terminator. Concavity makes the
///    refined piece-maxima sequence unimodal, so consecutive visits cannot
///    vault the peak.
/// 4. Stop and refine the current piece AND its forward neighbor with a
///    windowed ternary + dense sweep ([`Refiner::refine_window`]) so corner maxima
///    and edge-straddling peaks are considered.
///
/// The direction rule uses observed scores without a fixed wei tolerance. It
/// does not remove the smooth concavity/unimodality contract used by ternary
/// refinement and by skipping climbed-piece refinement, and is not an
/// arbitrary-non-unimodal global-optimality guarantee.
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

#[hotpath::measure(label = "concentrated_liquidity_solve.active_set")]
pub(super) fn solve_active_set_path(
    hops: &[PieceView],
    cfg: &SolveRuntimeConfig,
    env: Option<&[RefineBound]>,
    tele: Option<&WalkTelemetry>,
) -> WalkOutcome {
    let s_t0 = std::time::Instant::now();
    // The walk keeps a per-solve accumulator on the solving stack and threads
    // it by `&mut` to every writer it owns — the crossing helpers, the
    // word-profile build, and the event-census recorder included — so the
    // returned outcome carries THIS path's counters by construction, with no
    // ambient table to drain or to leak across solves. The one merge into the
    // engine collector happens here, at exit, under the crate's telemetry
    // feature.
    let mut tally = WalkTally::default();
    let out = solve_active_set_path_inner(hops, cfg, env, &mut tally);
    tally.add_solve_ns(u64::try_from(s_t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
    let stats = tally.walk_stats();
    let census_pieces = tally.take_census_pieces();
    #[cfg(feature = "telemetry")]
    if let Some(tele) = tele {
        tally.merge_into(tele);
    }
    #[cfg(not(feature = "telemetry"))]
    let _ = tele;
    WalkOutcome {
        result: out,
        refusal: None,
        stats,
        census_pieces,
    }
}

#[expect(clippy::too_many_lines)]
fn solve_active_set_path_inner(
    hops: &[PieceView],
    cfg: &SolveRuntimeConfig,
    env: Option<&[RefineBound]>,
    tally: &mut WalkTally,
) -> Option<WalkSolution> {
    /// Advance the landed tuple one piece past the window's right edge
    /// (the edge-bisection bracket is ≤4 wide, so scan a few steps).
    fn landed_beyond(
        hops: &[PieceView],
        right_edge: U256,
        ks: &[usize],
        tally: &mut WalkTally,
    ) -> Option<Vec<usize>> {
        let mut outcome = WalkPathOutcome::default();
        for d in 1u64..=8 {
            simulate_walk_path_into(
                right_edge.saturating_add(U256::from(d)),
                hops,
                tally,
                &mut outcome,
            );
            if landed_any_above(&outcome.landed, ks) {
                return Some(std::mem::take(&mut outcome.landed));
            }
        }
        None
    }

    /// Stop-time refinement: ternary + dense sweep over this piece's window
    /// AND over its immediate forward neighbor's window. Refinement of
    /// climbed-through pieces is skipped during the walk (their interior
    /// maxima cannot beat the walk's terminal region under concavity); the
    /// neighbor refinement covers a peak straddling the edge that the local
    /// direction probe could mis-attribute.
    #[expect(
        clippy::too_many_arguments,
        reason = "walk-domain refinement carry (window pair + hint + recorder + neighbor switch + runtime stance) is coherent as a flat signature"
    )]
    fn refine_at_stop(
        hops: &[PieceView],
        ks: &[usize],
        x_l: U256,
        x_r: Option<U256>,
        anchor: Option<U256>,
        rec: &mut WalkEvaluator,
        refine_neighbor: bool,
        cfg: &SolveRuntimeConfig,
        env: Option<&[RefineBound]>,
        tally: &mut WalkTally,
    ) {
        let Some(hint) = anchor else {
            return;
        };
        let hi_current = x_r.unwrap_or_else(|| {
            hint.saturating_mul(U256::from(4u64))
                .max(x_l.saturating_mul(U256::from(2u64)))
                .max(x_l.saturating_add(U256::from(1024u64)))
        });
        if x_l <= hi_current {
            Refiner::new(rec).refine_window(
                RefineWindow {
                    lo: x_l,
                    hi: hi_current,
                },
                anchor,
                cfg,
                env,
                tally,
            );
        }
        // Gated forward-neighbor refine: a full ternary +
        // grid over the neighbor window runs on climbing stops (edge can
        // straddle a peak) and — on falling stops — only when a cheap coarse
        // 33-point evidence grid finds the neighbor competitive within a
        // 0.1% grace band of the walk's best score. The ±64 straddle probe
        // can land past a thin neighbor piece, so a falling probe does NOT
        // bound the neighbor's interior — the coarse grid must adjudicate
        // instead. Direct wei corner probes at the
        // shared edge (F1 per-piece analogue) run in every fall.
        if !refine_neighbor {
            let Some(xr) = x_r else {
                return;
            };
            let Some(next) = landed_beyond(hops, xr, ks, tally) else {
                return;
            };
            let n_l = xr + U256::from(1u64);
            let n_r = piece_window_right_edge_evented(hops, &next, hint, None, None, cfg, tally).0;
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
                    rec.observe(xr + U256::from(2u64), ProbeKind::Candidate, tally);
                }
                let mut best_coarse = alloy::primitives::I256::MIN;
                for i in 0..33u64 {
                    let x = n_l + (span * U256::from(i)) / U256::from(32u64);
                    let o = rec.observe(x, ProbeKind::Candidate, tally);
                    let s = walk_profit_score(o.final_output, x);
                    if s > best_coarse {
                        best_coarse = s;
                    }
                }
                let grace = rec.top_score().max(alloy::primitives::I256::ZERO)
                    / alloy::primitives::I256::from_raw(alloy::primitives::U256::from(1_000u64));
                if best_coarse + grace >= rec.top_score() {
                    Refiner::new(rec).refine_window(
                        RefineWindow { lo: n_l, hi: n_hi },
                        None,
                        cfg,
                        env,
                        tally,
                    );
                }
            }
            return;
        }
        let Some(xr) = x_r else {
            return;
        };
        let Some(next) = landed_beyond(hops, xr, ks, tally) else {
            return;
        };
        let n_l = xr + U256::from(1u64);
        let n_r = piece_window_right_edge(hops, &next, hint, tally);
        let n_hi = n_r.unwrap_or_else(|| {
            hint.saturating_mul(U256::from(4u64))
                .max(n_l.saturating_mul(U256::from(2u64)))
                .max(n_l.saturating_add(U256::from(1024u64)))
        });
        if n_l <= n_hi {
            Refiner::new(rec).refine_window(
                RefineWindow { lo: n_l, hi: n_hi },
                None,
                cfg,
                env,
                tally,
            );
        }
    }

    if hops.is_empty() {
        return None;
    }

    let iteration_cap: usize = hops.iter().map(PieceView::piece_count).sum::<usize>() + 2;
    // The anchor fixed-point re-entry is scoped to wide/deep books; on the
    // small synthetic corpora it only perturbs equal-profit plateau tie-breaking.
    let wide_book = iteration_cap >= REFINE_ADAPTIVE_MIN_TOTAL_PIECES;
    let mut cursor = WalkCursor::new(hops.len());
    let mut rec = WalkEvaluator::new(hops);
    // Direct (non-recorder) probes share one outcome across walked pieces.
    let mut probe_outcome = WalkPathOutcome::default();
    // Anchor memoization: consecutive pieces share tuple prefixes, so the
    // Möbius fold reuses prefix partials (byte-exact — see
    // `ShiftedPieceComposer`). One composer per solve; `hops` is immutable
    // within a solve.
    let mut anchor_memo = crate::solve::mobius_shifted_piece::ShiftedPieceComposer::new();

    let single_piece_path = hops.iter().all(PieceView::single_piece);

    for _ in 0..iteration_cap {
        if let Err(terminal) = cursor.begin_piece() {
            cursor.apply(WalkTransition::Terminal(terminal));
            break;
        }
        tally.bump_pieces(1);
        let ks = cursor.ks();

        // Transitional anchor: extra candidates (±2 sweep) and edge-growth
        // hint; never trusted for the direction decision.
        // `DEGENBOT_WALK_ANCHOR_SWEEP=0` disables the ±2 probe set. The
        // anchor VALUE is still computed and used as the window-edge hint either way.
        let sweep = anchor_sweep_mode(cfg);
        let anchor_all_t0 = std::time::Instant::now();
        let anchor_build_t0 = std::time::Instant::now();
        let anchor_pieces = build_shifted_piece_hops(hops, ks);
        tally.add_anchor_build_ns(
            u64::try_from(anchor_build_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        let anchor_compose_t0 = std::time::Instant::now();
        let anchor_coeffs = anchor_memo.piece_coefficients(&anchor_pieces, ks);
        tally.add_anchor_compose_ns(
            u64::try_from(anchor_compose_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        let anchor_argmax_t0 = std::time::Instant::now();
        let anchor =
            crate::solve::mobius_shifted_piece::shifted_piece_model_optimal_input(&anchor_coeffs)
                .unwrap_or(U256::ZERO);
        tally.add_anchor_argmax_ns(
            u64::try_from(anchor_argmax_t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        tally.add_anchor_ns(u64::try_from(anchor_all_t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
        if !anchor.is_zero() {
            let anchor_t0 = tally.sims();
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
                rec.observe(candidate, ProbeKind::Candidate, tally);
            }
            let anchor_delta = tally.sims() - anchor_t0;
            tally.bump_anchor_sims(anchor_delta);
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
                    rec.observe(e, ProbeKind::Candidate, tally);
                    rec.observe(e - U256::from(1), ProbeKind::Candidate, tally);
                }
            }
            let hi = sat.map_or(anchor.max(U256::from(1024)), |e| e.max(anchor));
            if hi > U256::ZERO {
                let rq_mk = Mark::start(tally);
                Refiner::new(&mut rec).refine_window(
                    RefineWindow { lo: U256::ZERO, hi },
                    Some(anchor),
                    cfg,
                    env,
                    tally,
                );
                rq_mk.commit(tally, WalkSection::Refine);
            }
            cursor.apply(WalkTransition::Terminal(TerminalKind::UnboundedRefine));
            break;
        }

        // Window left edge: reuse the previous piece's right edge when
        // walking consecutively (scan a few steps forward); fall back to a
        // full bisection otherwise. (Section-census timers on every exit.)
        let le_mk = Mark::start(tally);
        let x_l = if ks.iter().all(|&k| k == 0) {
            U256::ZERO
        } else if let Some(prev) = cursor.prev_right_edge() {
            let mut found = None;
            for d in 1u64..=9 {
                let probe = prev + U256::from(d);
                simulate_walk_path_into(probe, hops, tally, &mut probe_outcome);
                if !probe_outcome
                    .landed
                    .iter()
                    .zip(ks.iter())
                    .any(|(a, &b)| *a < b)
                {
                    found = Some(probe);
                    break;
                }
            }
            match found {
                Some(x) => x,
                None => piece_window_left_edge(hops, ks, anchor, tally),
            }
        } else {
            piece_window_left_edge(hops, ks, anchor, tally)
        };

        // Skipped tuple: `x_l` lands strictly ABOVE `ks` in some component —
        // the lattice path never lands exactly on `ks`. Advance without
        // treating this as a real piece.
        {
            simulate_walk_path_into(x_l, hops, tally, &mut probe_outcome);
            if probe_outcome.landed != ks {
                if landed_any_above(&probe_outcome.landed, ks) {
                    let next = std::mem::take(&mut probe_outcome.landed);
                    cursor.apply(WalkTransition::SkippedTuple { next });
                    le_mk.commit(tally, WalkSection::Edge);
                    continue;
                }
                // landed BELOW ks means the left-edge search went wrong;
                // fall back to a full edge computation before giving up.
                let x_l_full = piece_window_left_edge(hops, ks, anchor, tally);
                simulate_walk_path_into(x_l_full, hops, tally, &mut probe_outcome);
                if probe_outcome.landed != ks {
                    if landed_any_above(&probe_outcome.landed, ks) {
                        let next = std::mem::take(&mut probe_outcome.landed);
                        cursor.apply(WalkTransition::SkippedTuple { next });
                        le_mk.commit(tally, WalkSection::Edge);
                        continue;
                    }
                    cursor.apply(WalkTransition::Terminal(TerminalKind::DegenerateNoRefine));
                    le_mk.commit(tally, WalkSection::Edge);
                    break;
                }
            }
        }
        le_mk.commit(tally, WalkSection::Edge);

        let re_mk = Mark::start(tally);
        let (x_r, right_confirm_hi) = piece_window_right_edge_evented(
            hops,
            ks,
            anchor,
            cursor.right_bracket().map(|b| b.0),
            cursor.right_bracket().map(|b| b.1),
            cfg,
            tally,
        );
        // Event census (`DEGENBOT_WALK_EVENT_CENSUS=1`): the nested
        // ceil-inversion's prediction vs this bisection bracket. No-op
        // (one bool load) when the gate is unset.
        event_census_record(hops, ks, x_r, right_confirm_hi, cfg, tally);
        re_mk.commit(tally, WalkSection::Redge);
        let Some(xr) = x_r else {
            // Terminal piece (unbounded right): refine and finish.
            let term_mk = Mark::start(tally);
            refine_at_stop(
                hops,
                ks,
                x_l,
                None,
                Some(anchor),
                &mut rec,
                false,
                cfg,
                env,
                tally,
            );
            term_mk.commit(tally, WalkSection::Refine);
            cursor.apply(WalkTransition::Terminal(TerminalKind::UnboundedRefine));
            break;
        };
        cursor.record_right_edge(xr, right_confirm_hi);

        // Direction uses rear parity, not a zero crossing or wei tolerance.
        // A negative local straddle is inconclusive, so widen the forward
        // observation until it reaches the rear score or the path terminates.
        let di_mk = Mark::start(tally);
        let decision = direction_at_edge(x_l, xr, |candidate| {
            let outcome = rec.observe(candidate, ProbeKind::Candidate, tally);
            DirectionObservation::from_output(
                outcome.final_output,
                candidate,
                outcome.model_domain_exhausted(),
            )
        });
        let climbing = decision.action == DirectionAction::Advance;
        let advance = if climbing {
            landed_beyond(hops, xr, ks, tally)
        } else {
            None
        };
        di_mk.commit(tally, WalkSection::Dir);
        if let Some(next) = advance {
            cursor.apply(WalkTransition::Advance { next });
            continue;
        }
        // Anchor fixed-point re-entry (`REFINE_ADAPTIVE_ANCHOR_OVERSHOOT`): a
        // fee-dominated entry can hold the marginal profit below zero for the
        // first stretches of a deep book, so the local ±64 straddle reads
        // "falling" while the composed smooth anchor — and the peak it
        // approximates — sits many pieces away. The immediate-neighbor refine
        // cannot reach it, so re-enter the walk at the anchor's landed tuple
        // and iterate: the piece's own smooth argmax is the next iterate, and
        // the fixed point is where the anchor lands in its own piece. The
        // visited guard bounds oscillation; the disproof-by-jump is followed
        // by a current-window refine because accumulated EVM flooring can
        // still move the realized peak inside the window being left.
        // The miss mode is a near-zero-width leading piece: the first crossing
        // is reached for ~0 input, so the piece-0 window is degenerate and the
        // ±64 straddle lands in the fee-dominated entry, misreading "falling".
        // Gate the re-entry on that geometry so ordinary wide-book stops keep
        // their recorded behaviour.
        let degenerate_lead = xr.saturating_sub(x_l) <= U256::from(REFINE_DENSE_SPAN);
        if REFINE_ADAPTIVE_ANCHOR_OVERSHOOT
            && wide_book
            && (cursor.anchor_reentry_active() || degenerate_lead)
            && !climbing
        {
            simulate_walk_path_into(anchor, hops, tally, &mut probe_outcome);
            if probe_outcome.landed != ks && !cursor.has_visited(&probe_outcome.landed) {
                let reentry_ks = std::mem::take(&mut probe_outcome.landed);
                // Run the full stop-time refine (current window + gated forward
                // neighbor) before leaving, so the extra exploration only adds
                // candidates and never forfeits the region a plain stop would
                // have refined.
                refine_at_stop(
                    hops,
                    ks,
                    x_l,
                    Some(xr),
                    Some(anchor),
                    &mut rec,
                    false,
                    cfg,
                    env,
                    tally,
                );
                cursor.apply(WalkTransition::AnchorReentry { next: reentry_ks });
                continue;
            }
        }
        let terminal = TerminalKind::StopRefine { climbing };
        let term_mk = Mark::start(tally);
        refine_at_stop(
            hops,
            ks,
            x_l,
            Some(xr),
            Some(anchor),
            &mut rec,
            terminal.refine_neighbor().unwrap_or(climbing),
            cfg,
            env,
            tally,
        );
        term_mk.commit(tally, WalkSection::Refine);
        cursor.apply(WalkTransition::Terminal(terminal));
        break;
    }
    cursor.apply(WalkTransition::Terminal(TerminalKind::CapExhausted));

    // Post-hoc telemetry: the walk now visits every initialized tick in
    // the swap direction (no `max_ranges` cap). The solver's own bounds
    // the iteration cap and bounded refinement searches prevent runaway cost,
    // but a pathological pool can still burn excessive pieces or
    // simulations. Surface those cases for diagnosis (not for screening —
    // the solve still completes and returns its result).
    let ws = tally.walk_stats();
    let over_threshold =
        ws.pieces > SOLVE_TELEMETRY_PIECES_WARN || ws.sims > SOLVE_TELEMETRY_SIMS_WARN;
    if over_threshold {
        let n_hops = hops.len();
        let mut range_counts = Vec::with_capacity(n_hops);
        let mut total_ranges = 0usize;
        for h in hops {
            let n = h.piece_count();
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

    rec.into_solution()
}
