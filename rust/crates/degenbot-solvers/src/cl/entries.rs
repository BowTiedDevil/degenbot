use std::sync::Arc;

use degenbot_math::v2::IntHopState;

use super::active_set::{solve_active_set_path, WalkHop, WalkOutcome};
use super::crossings::{build_cl_crossing_table, build_word_profiles, cl_walk_hop};
use super::memo::{walk_path_fingerprint, WalkMemo};
use super::{ClCrossingTable, ClProfileTable, IntV3TickRangeSequence};
use crate::runtime::SolveRuntimeConfig;

// Clippy: allow manual_ok_err in solve_v3_v3_piecewise match arms
// (the None/Err branches have side effects so let-else doesn't apply)

/// Solve a 2-hop V3-V3 arbitrage path with the active-set piecewise Möbius
/// walk (replaces the capped ending-range enumeration).
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
/// `hop_outputs[0]` = intermediate output from hop 1, `hop_outputs[1]` = final output.
#[must_use]
pub fn solve_v3_v3_piecewise(
    seq1: &IntV3TickRangeSequence,
    seq2: &IntV3TickRangeSequence,
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    solve_active_set_path(&[cl_walk_hop(seq1, None), cl_walk_hop(seq2, None)], cfg)
}

/// Solve an N-hop concentrated-liquidity arbitrage path with the active-set
/// piecewise Möbius walk (replaces the capped mixed-radix
/// ending-range enumeration — there is no tuple budget any more).
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
/// `hop_outputs[i]` = output after hop `i`.
/// One CL hop's prepared tables (crossing + word profiles): the intake value
/// for the solve entry — built ONCE per (pool, direction) by the projection,
/// or derived via [`ClSolveTables::derive`] by tableless callers (their cost).
#[derive(Clone)]
pub struct ClSolveTables {
    pub crossings: std::sync::Arc<ClCrossingTable>,
    pub profiles: std::sync::Arc<ClProfileTable>,
}

impl ClSolveTables {
    #[must_use]
    pub fn derive(seq: &IntV3TickRangeSequence) -> Self {
        Self {
            crossings: std::sync::Arc::new(build_cl_crossing_table(seq)),
            profiles: std::sync::Arc::new(build_word_profiles(&build_cl_crossing_table(seq))),
        }
    }
}

/// Convenience for callers that only have sequences (tests, examples,
/// golden-reference harnesses): derives the tables per call (cost is the
/// caller's) and runs the entry with no memo.
#[must_use]
pub fn derive_and_solve_cl_piecewise(
    sequences: &[&IntV3TickRangeSequence],
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    let prepared: Vec<ClSolveTables> = sequences.iter().map(|s| ClSolveTables::derive(s)).collect();
    solve_cl_piecewise(sequences, &prepared, None, cfg)
}

/// Stage-1 all-CL solve consuming the projection's precomputed crossing
/// tables + word-boundary profiles (built once per `(pool, direction)` in
/// `HopProjectionCache`, shared via `Arc` across paths). `crossings[k]` and
/// `profiles[k]` are parallel to `sequences[k]`. `crossings = None` builds the
/// crossing tables per call (offline mirror of [`solve_cl_piecewise`]).
/// THE all-CL solve entry: one interface taking the hop sequences, the
/// prepared tables (parallel to `sequences`), and the caller's
/// engine-owned cross-block memo handle.
#[must_use]
#[hotpath::measure(label = "cl_solve.int_solve_cl_path")]
pub fn solve_cl_piecewise(
    sequences: &[&IntV3TickRangeSequence],
    prepared: &[ClSolveTables],
    memo: Option<&WalkMemo>,
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    if sequences.is_empty() || prepared.len() != sequences.len() {
        return WalkOutcome::none();
    }
    // Cross-block composition memo: the fingerprint is the exact correctness
    // key (the tables are pure derivations of the sequence), so an identical
    // key cannot carry a stale result. `None` memo = disabled run never pays
    // the fingerprint or the lock.
    if let Some(memo) = memo {
        if memo.active() {
            let fp = walk_path_fingerprint(sequences);
            if let Some(hit) = memo.probe(fp) {
                return WalkOutcome::from_result(Some(hit));
            }
            let outcome = solve_cl_piecewise_inner(sequences, prepared, cfg);
            memo.note_cost(fp, outcome.stats.sims as u64);
            memo.store(fp, outcome.result.as_ref());
            return outcome;
        }
    }
    solve_cl_piecewise_inner(sequences, prepared, cfg)
}

/// The memo-less solve body (the memo hook is the only difference).
fn solve_cl_piecewise_inner(
    sequences: &[&IntV3TickRangeSequence],
    prepared: &[ClSolveTables],
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    let hops: Vec<WalkHop> = (0..sequences.len())
        .map(|i| WalkHop::Cl {
            crossings: Arc::clone(&prepared[i].crossings),
            profiles: Arc::clone(&prepared[i].profiles),
        })
        .collect();
    solve_active_set_path(&hops, cfg)
}

// ---------------------------------------------------------------------------
// Integer V3 Exact Solver
// ---------------------------------------------------------------------------

/// Solve a mixed V2-V3 arbitrage path with the active-set piecewise Möbius
/// walk (replaces the capped ending-range enumeration over the
/// V3 side — there is no tuple budget any more).
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
/// `hop_outputs[0]` = output from the first hop, `hop_outputs[1]` = output from the second.
#[must_use]
pub fn solve_mixed_v2_v3_piecewise(
    v2_hops: &[IntHopState],
    v3_sequence: &IntV3TickRangeSequence,
    v3_first: bool,
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    let mut hops: Vec<WalkHop> = Vec::with_capacity(v2_hops.len() + 1);
    let cl_hop = cl_walk_hop(v3_sequence, None);
    if v3_first {
        hops.push(cl_hop);
        hops.extend(v2_hops.iter().map(WalkHop::ConstantProduct));
    } else {
        hops.extend(v2_hops.iter().map(WalkHop::ConstantProduct));
        hops.push(cl_hop);
    }
    solve_active_set_path(&hops, cfg)
}

/// Solve an N-hop mixed V2 + CL (V3/V4) arbitrage path with the active-set
/// piecewise Möbius walk (replaces the capped mixed-radix
/// enumeration over CL ending ranges — there is no tuple budget any more).
///
/// - `v2_hops[i]`: V2 hop state at position `i` (`None` for CL positions)
/// - `cl_sequences[i]`: CL tick-range sequence at position `i` (`None` for V2 positions)
/// - `cl_crossings[i]`/`cl_profiles[i]`: cached projection tables (`None` for V2
///   positions; `cl_crossings = None` builds tables per call for offline callers)
/// - `hop_order`: true = V2 hop, false = CL hop
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
/// THE mixed V2+CL solve entry — prepared tables ride [`ClSolveTables`] per CL
/// hop position; `None` derives them at the caller's cost, the
/// offline/replay shape.
#[must_use]
#[hotpath::measure(label = "cl_solve.exact_solve_mixed_path_n")]
pub fn solve_mixed_piecewise(
    v2_hops: &[Option<IntHopState>],
    // RLVDUP T1: borrowed sequences - the walk only READS a sequence
    // (fallback table build); the previous owned signature forced every
    // caller to deep-copy the ranges Vec per solve.
    cl_sequences: &[Option<&IntV3TickRangeSequence>],
    cl_prepared: &[Option<ClSolveTables>],
    hop_order: &[bool], // true = V2, false = CL
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    let n_hops = hop_order.len();
    if n_hops < 2 || v2_hops.len() != n_hops || cl_sequences.len() != n_hops {
        return WalkOutcome::none();
    }
    if cl_prepared.len() != n_hops {
        return WalkOutcome::none();
    }
    let mut hops: Vec<WalkHop> = Vec::with_capacity(n_hops);
    for (i, &is_v2) in hop_order.iter().enumerate() {
        if is_v2 {
            let Some(v2) = v2_hops[i].as_ref() else {
                return WalkOutcome::none();
            };
            hops.push(WalkHop::ConstantProduct(v2));
        } else {
            let Some(seq) = cl_sequences[i] else {
                return WalkOutcome::none();
            };
            let crossings;
            let profiles;
            if let Some(prep) = cl_prepared[i].as_ref() {
                crossings = Arc::clone(&prep.crossings);
                profiles = Arc::clone(&prep.profiles);
            } else {
                crossings = Arc::new(build_cl_crossing_table(seq));
                profiles = Arc::new(build_word_profiles(&crossings));
            }
            hops.push(WalkHop::Cl {
                crossings,
                profiles,
            });
        }
    }
    solve_active_set_path(&hops, cfg)
}
