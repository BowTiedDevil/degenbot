use degenbot_math::v2::IntHopState;

use super::active_set::{solve_active_set_path, PieceView, WalkOutcome};
use super::crossings::{
    build_cl_crossing_table, build_word_profiles, cl_walk_hop, cl_walk_hop_cached,
};
use super::memo::{walk_path_fingerprint, WalkMemo};
use super::{ClCrossingTable, ClProfileTable, IntV3TickRangeSequence};
use crate::profit_envelope::PathBoundLines;
use crate::runtime::SolveRuntimeConfig;

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

/// Build one CL hop's walk view from a sequence and its optional prepared
/// tables. `Some` O(1)-clones the projection's `Arc` tables; `None` builds
/// them here. The single seam both solve entries assemble CL hops through.
fn cl_hop_view<'a>(
    seq: &'a IntV3TickRangeSequence,
    prepared: Option<&ClSolveTables>,
) -> PieceView<'a> {
    match prepared {
        Some(p) => cl_walk_hop_cached(seq, Some(&p.crossings), Some(&p.profiles)),
        None => cl_walk_hop(seq, None),
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
    solve_cl_piecewise(sequences, &prepared, None, cfg, None)
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
    env: Option<&PathBoundLines>,
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
            let outcome = solve_cl_piecewise_inner(sequences, prepared, cfg, env);
            memo.note_cost(fp, outcome.stats.sims as u64);
            memo.store(fp, outcome.result.as_ref());
            return outcome;
        }
    }

    solve_cl_piecewise_inner(sequences, prepared, cfg, env)
}

/// The memo-less solve body (the memo hook is the only difference).
fn solve_cl_piecewise_inner(
    sequences: &[&IntV3TickRangeSequence],
    prepared: &[ClSolveTables],
    cfg: &SolveRuntimeConfig,
    env: Option<&PathBoundLines>,
) -> WalkOutcome {
    let hops: Vec<PieceView> = sequences
        .iter()
        .zip(prepared)
        .map(|(seq, tables)| cl_hop_view(seq, Some(tables)))
        .collect();
    solve_active_set_path(&hops, cfg, env)
}

// ---------------------------------------------------------------------------
// Integer V3 Exact Solver
// ---------------------------------------------------------------------------

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
    // Borrowed sequences: the walk only READS a sequence (fallback table
    // build), so an owned signature would make every caller deep-copy the
    // ranges Vec per solve.
    cl_sequences: &[Option<&IntV3TickRangeSequence>],
    cl_prepared: &[Option<ClSolveTables>],
    hop_order: &[bool], // true = V2, false = CL
    cfg: &SolveRuntimeConfig,
    env: Option<&PathBoundLines>,
) -> WalkOutcome {
    let n_hops = hop_order.len();
    if n_hops < 2 || v2_hops.len() != n_hops || cl_sequences.len() != n_hops {
        return WalkOutcome::none();
    }
    if cl_prepared.len() != n_hops {
        return WalkOutcome::none();
    }

    let mut hops: Vec<PieceView> = Vec::with_capacity(n_hops);
    for (i, &is_v2) in hop_order.iter().enumerate() {
        if is_v2 {
            let Some(v2) = v2_hops[i].as_ref() else {
                return WalkOutcome::none();
            };
            hops.push(PieceView::constant_product(v2));
        } else {
            let Some(seq) = cl_sequences[i] else {
                return WalkOutcome::none();
            };
            hops.push(cl_hop_view(seq, cl_prepared[i].as_ref()));
        }
    }

    solve_active_set_path(&hops, cfg, env)
}
