use std::sync::Arc;

use alloy::primitives::U256;

use super::active_set::{landed_any_above, simulate_walk_path, WalkHop};
use super::telemetry::{
    add_pred_ns, bump_event_solver_fallbacks, bump_event_solver_ok, bump_left_edge_sims,
    bump_right_edge_sims, observe_max_dense_words,
};
use super::word_profile::{word_profile_min_input_for_output, ClWordProfile};
use super::{ClCrossingTable, ClProfileTable, IntTickRangeCrossing, IntV3TickRangeSequence};
use crate::runtime::SolveRuntimeConfig;

/// Smallest input into this CL hop, while it lands exactly in the crossing's
/// ending range, whose realized output is >= `w`. `None` when `w` exceeds
/// the landing's capacity.
fn cl_hop_min_input_for_output(
    crossing: &IntTickRangeCrossing,
    profile: Option<&ClWordProfile>,
    w: U256,
) -> Option<U256> {
    if w <= crossing.crossing_output {
        return Some(crossing.crossing_gross_input);
    }
    let w_ending = w - crossing.crossing_output;
    // Profiles are byte-equivalent to the linear `simulate_v3_range_swap`
    // walk, so the inversion routes through the profile tables.
    let r = if let Some(p) = profile {
        word_profile_min_input_for_output(p, w_ending)?
    } else {
        let built = ClWordProfile::build(&crossing.ending_range)?;
        word_profile_min_input_for_output(&built, w_ending)?
    };
    crossing.crossing_gross_input.checked_add(r)
}

/// The predicted first-above input for tuple `ks`: the minimum over CL hops
/// of the nested exact-out inversion — hop `i`'s next-boundary gross demand
/// `T_i` propagated upstream through each hop's `min-input-for-output`
/// (each within its current landing, guarded by the next-boundary gross so
/// a preempted upstream exit skips the candidate). `None` = terminal piece
/// (no hop bounds the region).
///
/// This is the *exact* realized-chain inversion under the floor-cancel
/// lemma; the loop-15 census measures how often it agrees with the bisection
/// ground truth on captured states.
pub(super) fn walk_event_first_above_predicted(hops: &[WalkHop], ks: &[usize]) -> Option<U256> {
    let p_t0 = std::time::Instant::now();
    let out = walk_event_first_above_predicted_inner(hops, ks);
    add_pred_ns(u64::try_from(p_t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
    out
}

fn walk_event_first_above_predicted_inner(hops: &[WalkHop], ks: &[usize]) -> Option<U256> {
    let mut best: Option<U256> = None;
    for i in 0..hops.len() {
        let crossings = match &hops[i] {
            WalkHop::ConstantProduct(_) => continue,
            WalkHop::Cl { crossings, .. } => crossings,
        };
        let Some(next) = crossings.get(ks[i] + 1) else {
            continue;
        };
        let mut demand = next.crossing_gross_input;
        if demand.is_zero() {
            // Zero-cost boundary: the tuple is already exceeded at x = 0.
            return Some(U256::ZERO);
        }
        let mut reachable = true;
        for h in (0..i).rev() {
            if demand.is_zero() {
                break;
            }
            match &hops[h] {
                WalkHop::ConstantProduct(state) => {
                    if let Ok(z) = state.swap_exact_out(demand) {
                        demand = z;
                    } else {
                        reachable = false;
                        break;
                    }
                }
                WalkHop::Cl {
                    crossings,
                    profiles,
                } => {
                    let k = ks[h];
                    let crossing = &crossings[k];
                    let Some(z) =
                        cl_hop_min_input_for_output(crossing, profiles[k].as_deref(), demand)
                    else {
                        reachable = false;
                        break;
                    };
                    if let Some(next_boundary) = crossings.get(k + 1) {
                        if z >= next_boundary.crossing_gross_input {
                            // The upstream hop exits its landing before the
                            // demand is met — its own candidate (in this set)
                            // preempts this one.
                            reachable = false;
                            break;
                        }
                    }
                    demand = z;
                }
            }
        }
        if reachable {
            best = Some(best.map_or(demand, |b| b.min(demand)));
        }
    }
    best
}

pub(super) fn landed_ending_range_index(
    crossings: &[IntTickRangeCrossing],
    available: U256,
) -> usize {
    debug_assert!(!crossings.is_empty());
    debug_assert!(crossings[0].crossing_gross_input.is_zero());
    crossings.partition_point(|c| c.crossing_gross_input <= available) - 1
}

/// First input of the tuple-`ks` window: the smallest `x` whose landed tuple
/// is componentwise ≥ `ks` (0 when `ks` is all zeros).
///
/// `landed(x)` is componentwise non-decreasing in `x`, so the predicate is
/// monotone and bisection is sound.
pub(super) fn piece_window_left_edge(hops: &[WalkHop], ks: &[usize], hint: U256) -> U256 {
    if ks.iter().all(|&k| k == 0) {
        return U256::ZERO;
    }
    // Predicate: every landed component ≥ ks. lo = 0 is false (landed(0) is
    // all-zeros and ks has a positive component).
    let mut lo = U256::ZERO;
    let mut hi = hint.max(U256::ONE);
    for _ in 0..256 {
        bump_left_edge_sims(1);
        let landed = simulate_walk_path(hi, hops).landed;
        if !landed.iter().zip(ks.iter()).any(|(a, &b)| *a < b) {
            break; // predicate true
        }
        lo = hi;
        hi = match hi.checked_mul(U256::from(2u64)) {
            Some(v) => v,
            None => break, // domain edge; treat the bracket as terminal
        };
    }
    // Bisect [lo (predicate false), hi (predicate true)] to a ≤64 bracket,
    // then scan for the exact first in-window input.
    while hi.saturating_sub(lo) > U256::from(64u64) {
        let mid = lo + (hi - lo) / U256::from(2u64);
        bump_left_edge_sims(1);
        let landed = simulate_walk_path(mid, hops).landed;
        if landed.iter().zip(ks.iter()).any(|(a, &b)| *a < b) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut x = lo + U256::from(1u64);
    while x < hi {
        bump_left_edge_sims(1);
        let landed = simulate_walk_path(x, hops).landed;
        if !landed.iter().zip(ks.iter()).any(|(a, &b)| *a < b) {
            return x;
        }
        x += U256::from(1u64);
    }
    hi
}

/// Last input of the `≤ ks` region: the largest `x` whose landed tuple is
/// componentwise ≤ `ks`. Returns `None` when the region is unbounded (no hop
/// crosses any further — the piece is terminal).
///
/// Sound by the same monotonicity argument as [`piece_window_left_edge`].
pub(super) fn piece_window_right_edge(hops: &[WalkHop], ks: &[usize], hint: U256) -> Option<U256> {
    piece_window_right_edge_seeded(hops, ks, hint, None, None).0
}

pub(super) fn piece_window_right_edge_evented(
    hops: &[WalkHop],
    ks: &[usize],
    hint: U256,
    lo_seed: Option<U256>,
    hi_seed: Option<U256>,
    cfg: &SolveRuntimeConfig,
) -> (Option<U256>, U256) {
    // Rollout gate: the runtime config's stance forces the legacy grow +
    // bisection (A/B toggle; packed by the owner at construction).
    if cfg.event_solver_legacy {
        return piece_window_right_edge_seeded(hops, ks, hint, lo_seed, hi_seed);
    }
    if let Some(pa) = walk_event_first_above_predicted(hops, ks) {
        let usable = !pa.is_zero() && {
            let above = simulate_walk_path(pa, hops).landed;
            landed_any_above(&above, ks)
        };
        if usable {
            let below = simulate_walk_path(pa - U256::ONE, hops).landed;
            bump_right_edge_sims(2);
            if !landed_any_above(&below, ks) {
                bump_event_solver_ok(1);
                return (Some(pa - U256::ONE), pa);
            }
        }
    }
    bump_event_solver_fallbacks(1);
    piece_window_right_edge_seeded(hops, ks, hint, lo_seed, hi_seed)
}

/// [`piece_window_right_edge`] with warm-started bisection brackets.
///
/// Consecutive walked pieces advance `ks` componentwise and `landed(x)` is
/// componentwise non-decreasing in x, so the previous piece's right edge is
/// always a lower-bound seed for the next piece's edge — no probe needed.
/// The hi seed is the previous bisection's tight confirmed-above bound
/// (<= edge + 4 wei); the first probe of the grow loop doubles as its
/// validation (a stale seed simple falls into the standard grow loop from
/// there, never below the cold-path starting hi). Returns `(edge, hi_to_reuse)`
/// where `hi_to_reuse` is the final confirmed-above bound — the caller feeds
/// it back on the next piece.
fn piece_window_right_edge_seeded(
    hops: &[WalkHop],
    ks: &[usize],
    hint: U256,
    lo_seed: Option<U256>,
    hi_seed: Option<U256>,
) -> (Option<U256>, U256) {
    let mut lo = U256::ZERO; // landed(0) = all zeros ≤ ks for any ks
    let mut hi = hint.max(U256::ONE);
    let mut confirmed = false;

    // Lo warm start needs NO probe: ks advances componentwise between
    // consecutive pieces and landed(x) is componentwise non-decreasing in x,
    // so the previous piece's edge always lands ≤ the current ks. The seeded
    // lo can only be invalid if ks went DOWNWARD, which the walk never does
    // (landing scans return strictly-forward tuples; a jump tuple replaces ks
    // with its own landing, still ≥ landed(xr_prev)).
    if let Some(lseed) = lo_seed.filter(|s| !s.is_zero()) {
        lo = lseed;
    }
    // Hi seed is the previous bisection's tight confirmed-above bound
    // (≤ edge + 4): cold-quality lower bound so a stale seed cannot make
    // the grow loop start lower than the cold path would.
    if let Some(hseed) = hi_seed {
        hi = hseed.max(hint.max(U256::ONE));
    }
    for _ in 0..256 {
        bump_right_edge_sims(1);
        let landed = simulate_walk_path(hi, hops).landed;
        if landed_any_above(&landed, ks) {
            confirmed = true;
            break;
        }
        lo = hi;
        hi = match hi.checked_mul(U256::from(2u64)) {
            Some(v) => v,
            None => break,
        };
    }
    if !confirmed {
        return (None, hi); // unbounded region — terminal piece
    }
    // Bisect to a ≤4 bracket: lo is the largest known ≤ ks input.
    while hi.saturating_sub(lo) > U256::from(4u64) {
        let mid = lo + (hi - lo) / U256::from(2u64);
        bump_right_edge_sims(1);
        let landed = simulate_walk_path(mid, hops).landed;
        if landed_any_above(&landed, ks) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (Some(lo), hi)
}

/// Pre-compute the crossing data for every ending-range index of a CL
/// sequence.
#[hotpath::measure(label = "cl_solve.build_crossing_table")]
pub(super) fn build_crossing_table(seq: &IntV3TickRangeSequence) -> Vec<IntTickRangeCrossing> {
    // O(N) single pass via `crossings()`; byte-identical results (proven by
    // `crossings_matches_per_k_compute_crossing`).
    seq.crossings()
}

/// EVERY nonzero-liquidity range carries a profile (empty-boundary ranges
/// degenerate to one constant terminal step). Per-sim walk cost dominated live heavy
/// paths (7-17k word steps/path in release replay); profile queries collapse
/// each sim to a partition search against cumulative constants + ONE live
/// landing step, and full landings return constants with zero wide math.
/// Build is O(K) once per (pool, direction) per state resolve.
/// Q3: dense word-boundary profiles are load-bearing on real sparse pools —
/// when a range reaches this many boundaries the walk reports
/// `WalkStats::max_dense_words` and the CONSUMER decides to alert.
pub const DENSE_OBSERVE_THRESHOLD: usize = 64;

/// Precomputed forward word-boundary profiles, parallel to `crossings`. A dense
/// range re-walks the same word-boundary prefix on nearly every one of a path's
/// ~`sims` evaluations, so we precompute its forward profile once; a light range
/// stays `None` (linear walk, zero build overhead). Each dense range's profile is
/// `Arc`-backed so it is shared - not re-cloned - across every path that reuses
/// it (the hop-projection memoization).
#[hotpath::measure(label = "cl_solve.build_word_profiles")]
pub(super) fn build_word_profiles(
    crossings: &[IntTickRangeCrossing],
) -> Vec<Option<Arc<ClWordProfile>>> {
    for c in crossings {
        let n = c.ending_range.word_boundary_prices.len();
        observe_max_dense_words(n);
        // KEEP ledger (Stage-1 sharing): a dense profile builds ONCE per
        // (pool, direction) and is Arc-shared across every path reusing the
        // projection — build cost amortized over paths, O(1) clone per path,
        // memory bounded to one profile per dense range. That amortization is
        // the cost-accounted basis for KEEP given the M=27 live dense pools.
    }
    crossings
        .iter()
        .map(|c| ClWordProfile::build(&c.ending_range).map(Arc::new))
        .collect()
}

/// Cache-less crossing data for a CL sequence (every ending-range index).
/// The projection wraps this in an `Arc` and stores it on the resolved hop so
/// the same table is shared across every path reusing the pool.
#[must_use]
pub fn build_cl_crossing_table(seq: &IntV3TickRangeSequence) -> Vec<IntTickRangeCrossing> {
    build_crossing_table(seq)
}

/// Cache-less dense-range profiles for a CL sequence (offline replays and the
/// direct `solve_cl_piecewise` path, which build per call). Result is parallel to
/// `seq.ranges` (`None` for ranges below `WORD_PROFILE_THRESHOLD`).
#[must_use]
pub fn build_cl_word_profiles(seq: &IntV3TickRangeSequence) -> ClProfileTable {
    build_word_profiles(&build_crossing_table(seq))
}

/// Dense-range profiles derived from an already-built crossing table. The
/// projection builds crossings once and feeds them here so the O(n²) crossing
/// table is not rebuilt a second time just to derive word profiles.
#[must_use]
pub fn build_cl_word_profiles_from_crossings(crossings: &[IntTickRangeCrossing]) -> ClProfileTable {
    build_word_profiles(crossings)
}

/// Build a `WalkHop::Cl` (crossing table + word-boundary profiles) for one CL
/// sequence - the single place a CL walk hop is assembled. `crossings`/`profiles`
/// are precomputed projection tables (Arc-shared through the hop memoization),
/// cloned in O(1); `None` builds that table here.
#[hotpath::measure(label = "cl_solve.cl_walk_hop")]
fn cl_walk_hop_cached<'a>(
    seq: &'a IntV3TickRangeSequence,
    crossings: Option<&Arc<ClCrossingTable>>,
    profiles: Option<&Arc<ClProfileTable>>,
) -> WalkHop<'a> {
    let crossings = match crossings {
        Some(c) => Arc::clone(c),
        None => Arc::new(build_crossing_table(seq)),
    };
    let profiles = match profiles {
        Some(p) => Arc::clone(p),
        None => Arc::new(build_word_profiles(&crossings)),
    };
    WalkHop::Cl {
        crossings,
        profiles,
    }
}

/// Cache-less variant of [`cl_walk_hop_cached`] for offline callers that only
/// have a sequence (builds both tables per call).
pub(super) fn cl_walk_hop<'a>(
    seq: &'a IntV3TickRangeSequence,
    profiles: Option<&Arc<ClProfileTable>>,
) -> WalkHop<'a> {
    cl_walk_hop_cached(seq, None, profiles)
}
