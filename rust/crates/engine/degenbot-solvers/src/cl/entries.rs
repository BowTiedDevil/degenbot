use degenbot_math::v2::IntHopState;

use super::active_set::{solve_active_set_path, PieceView, WalkOutcome};
use super::crossings::{
    build_cl_crossing_table, build_cl_word_profiles_from_crossings, cl_walk_hop_cached,
};
use super::memo::{walk_mixed_path_fingerprint, walk_path_fingerprint, MemoProbe, WalkMemo};
use super::{ClCrossingTable, ClProfileTable, IntV3TickRangeSequence};
use crate::profit_envelope::PathBoundLines;
use crate::runtime::SolveRuntimeConfig;

/// One CL hop's DERIVATION value: the tick-range sequence it was derived from,
/// paired with the crossing table + word profiles built from that sequence,
/// built ONCE per (pool, direction) by the projection.
///
/// This is the projection-carried shape [`ResolvedHop::V3`/`V4`] carries (see
/// `mixed::ResolvedHop`) AND the solve entries' intake: the sequence and its
/// tables travel as one value, so the pairing the deleted gate defended is
/// structural — and that structural pairing is this value's whole point.
#[derive(Clone, Debug)]
pub struct PreparedClHop {
    /// The tick-range sequence these tables were derived from, `Arc`-shared
    /// across every path reusing the projection.
    pub int_seq: std::sync::Arc<IntV3TickRangeSequence>,
    /// The per-ending-range crossing table ([`build_cl_crossing_table`]).
    pub crossings: std::sync::Arc<ClCrossingTable>,
    /// The dense-range word-boundary profiles, derived from `crossings` (the
    /// fused-epoch winner: ONE crossing build derives both artifacts).
    pub profiles: std::sync::Arc<ClProfileTable>,
}

impl PreparedClHop {
    /// Derive the crossing table + word profiles from `int_seq` — the
    /// projection's construction.
    ///
    /// The crossing table is built directly and the word profiles derived from
    /// that same table (not rebuilt from the sequence), so the O(n²) crossing
    /// build runs exactly once per (pool, direction).
    #[must_use]
    pub fn derive(int_seq: std::sync::Arc<IntV3TickRangeSequence>) -> Self {
        let crossings = std::sync::Arc::new(build_cl_crossing_table(&int_seq));
        let profiles = std::sync::Arc::new(build_cl_word_profiles_from_crossings(&crossings));
        Self {
            int_seq,
            crossings,
            profiles,
        }
    }

    /// Wrap an already-built sequence + tables (offline/replay callers that
    /// build the tables themselves).
    #[must_use]
    pub fn from_parts(
        int_seq: std::sync::Arc<IntV3TickRangeSequence>,
        crossings: std::sync::Arc<ClCrossingTable>,
        profiles: std::sync::Arc<ClProfileTable>,
    ) -> Self {
        Self {
            int_seq,
            crossings,
            profiles,
        }
    }
}

/// One hop of a mixed V2+CL path as the walk consumes it — the mixed entry's
/// WHOLE intake, one borrowed view per position: either a V2 constant-product
/// state or the projection's prepared CL hop. One slice of these makes the
/// positional pairing (which position is which family, and where the prepared
/// tables live) structural, so the parallel positional slices and the
/// per-solve table rebuild they forced are gone.
#[derive(Clone, Copy)]
pub enum MixedHopView<'a> {
    /// V2 constant-product hop: the walk borrows the resolved state.
    V2(&'a IntHopState),
    /// CL hop: the projection's [`PreparedClHop`] derivation value.
    Cl(&'a PreparedClHop),
}

impl<'a> MixedHopView<'a> {
    /// This hop's walk [`PieceView`]: V2 borrows the state; CL O(1)-clones the
    /// projection's `Arc` tables (nothing is derived or rebuilt per solve).
    fn piece_view(self) -> PieceView<'a> {
        match self {
            Self::V2(state) => PieceView::constant_product(state),
            Self::Cl(prepared) => cl_walk_hop_cached(
                prepared.int_seq.as_ref(),
                Some(&prepared.crossings),
                Some(&prepared.profiles),
            ),
        }
    }
}

/// Shared memo replay arm for both solve entries: probe the cross-block
/// composition memo at the caller's exact-key `fp` (each entry computes and
/// documents its own fingerprint). `Some` means a `Hit` or `Negative`
/// answered the probe and the caller returns the outcome as-is, skipping the
/// walk; `None` (`Miss`) falls through to hop assembly + solve +
/// [`memo_commit`].
fn memo_replay(memo: &WalkMemo, fp: u128) -> Option<WalkOutcome> {
    match memo.probe(fp) {
        MemoProbe::Hit(hit) => Some(WalkOutcome::from_result(Some(hit))),
        // Cached negative: this composition already walked to None under
        // identical inputs (exact-key soundness documented at the Negative
        // arm in memo.rs). Skip the inner solve, note_cost, and store — the
        // negative is already stored.
        MemoProbe::Negative => Some(WalkOutcome::none()),
        MemoProbe::Miss => None,
    }
}

/// Shared memo commit arm for both solve entries: charge the walk's cost
/// against the memo slot and store the outcome's result (`None` included —
/// that is what turns a later identical probe into a cached negative).
fn memo_commit(memo: &WalkMemo, fp: u128, outcome: &WalkOutcome) {
    memo.note_cost(fp, outcome.stats.sims as u64);
    memo.store(fp, outcome.result.as_ref());
}

/// Convenience for callers that only have sequences (tests, examples,
/// golden-reference harnesses): derives each hop's [`PreparedClHop`] per call
/// (cost is the caller's) and runs the entry with no memo.
#[must_use]
pub fn derive_and_solve_cl_piecewise(
    sequences: &[&IntV3TickRangeSequence],
    cfg: &SolveRuntimeConfig,
) -> WalkOutcome {
    let prepared: Vec<PreparedClHop> = sequences
        .iter()
        .map(|s| PreparedClHop::derive(std::sync::Arc::new(IntV3TickRangeSequence::clone(s))))
        .collect();
    solve_cl_piecewise(&prepared, None, cfg, None)
}

/// THE all-CL solve entry: one interface taking the projection's prepared hops
/// and the caller's engine-owned cross-block memo handle.
///
/// `prepared[k]` IS the hop — the tick-range sequence it was derived from plus
/// the crossing table and word profiles built from it (built once per
/// `(pool, direction)` in `HopProjectionCache`, shared via `Arc` across paths)
/// — so the table source and the tables travel as one value and no positional
/// re-derivation is needed.
#[must_use]
#[hotpath::measure(label = "cl_solve.int_solve_cl_path")]
pub fn solve_cl_piecewise(
    prepared: &[PreparedClHop],
    memo: Option<&WalkMemo>,
    cfg: &SolveRuntimeConfig,
    env: Option<&PathBoundLines>,
) -> WalkOutcome {
    if prepared.is_empty() {
        return WalkOutcome::none();
    }

    // Cross-block composition memo: the fingerprint is the exact correctness
    // key (the tables are pure derivations of their sequence), so an identical
    // key cannot carry a stale result. The probe runs BEFORE hop assembly — the
    // key needs only the hops' sequences, so a `Hit` or `Negative` returns
    // without paying the `PieceView` builds. `None` memo = disabled run never
    // pays the fingerprint or the lock.
    let mut commit = None;
    if let Some(memo) = memo {
        if memo.active() {
            let seqs: Vec<&IntV3TickRangeSequence> =
                prepared.iter().map(|h| h.int_seq.as_ref()).collect();
            let fp = walk_path_fingerprint(&seqs);
            if let Some(outcome) = memo_replay(memo, fp) {
                return outcome;
            }
            commit = Some((memo, fp));
        }
    }

    let outcome = solve_cl_piecewise_inner(prepared, cfg, env);
    if let Some((memo, fp)) = commit {
        memo_commit(memo, fp, &outcome);
    }
    outcome
}

/// The memo-less solve body (the memo hook is the only difference).
fn solve_cl_piecewise_inner(
    prepared: &[PreparedClHop],
    cfg: &SolveRuntimeConfig,
    env: Option<&PathBoundLines>,
) -> WalkOutcome {
    let hops: Vec<PieceView> = prepared
        .iter()
        .map(|h| cl_walk_hop_cached(h.int_seq.as_ref(), Some(&h.crossings), Some(&h.profiles)))
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
/// `hops` is ONE borrowed [`MixedHopView`] per position — `V2(&IntHopState)`
/// for a constant-product position, `Cl(&PreparedClHop)` for a prepared CL
/// hop — so which family each position carries, and where the prepared
/// tables live, are structural rather than positional. Offline/replay callers
/// that only hold sequences derive a [`PreparedClHop`] per hop (their cost).
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
///
/// THE mixed V2+CL solve entry. `memo` (mirroring [`solve_cl_piecewise`]
/// position-for-position) is the caller's engine-owned cross-block
/// composition memo: the entry probes by [`walk_mixed_path_fingerprint`]
/// BEFORE hop assembly — the key needs only the hop states, sequences, and
/// order, so a cached `Hit` replays the solution and a cached `Negative`
/// skips the walk, `note_cost`, and store without paying for assembly; a
/// `Miss` falls through to hop assembly + solve + `note_cost` + `store`.
/// Pure-V2 compositions (no CL hops at all) route through this entry from
/// `mixed::solve` too, so they gain memo coverage here as well. (The
/// per-CL-hop table source gate this entry once ran is gone: the projection
/// hands each hop a prepared value paired with its own sequence, so the
/// pairing is structural and no table is rebuilt per solve.)
#[must_use]
#[hotpath::measure(label = "cl_solve.exact_solve_mixed_path_n")]
pub fn solve_mixed_piecewise(
    hops: &[MixedHopView<'_>],
    memo: Option<&WalkMemo>,
    cfg: &SolveRuntimeConfig,
    env: Option<&PathBoundLines>,
) -> WalkOutcome {
    if hops.len() < 2 {
        return WalkOutcome::none();
    }

    // Cross-block composition memo: the mixed fingerprint folds every
    // position's family tag + hop state (V2: the full `IntHopState` the
    // constant-product view consumes; CL: the same per-range folding the
    // all-CL key uses), so the key is exact for mixed compositions — cfg is
    // fixed per engine lifetime and env (PathBoundLines) is a pure
    // derivation of hop state + cfg. The probe runs BEFORE hop assembly — the
    // key needs only the hop states, sequences, and order, so a `Hit` or
    // `Negative` returns without paying the `PieceView` builds. `None` memo =
    // disabled run never pays the fingerprint or the lock. (The key is
    // re-projected from the view here: the V2 state clone is the fold's, not
    // the walk's.)
    let mut commit = None;
    if let Some(memo) = memo {
        if memo.active() {
            let v2_hops: Vec<Option<IntHopState>> = hops
                .iter()
                .map(|h| match h {
                    MixedHopView::V2(state) => Some((*state).clone()),
                    MixedHopView::Cl(_) => None,
                })
                .collect();
            let cl_sequences: Vec<Option<&IntV3TickRangeSequence>> = hops
                .iter()
                .map(|h| match h {
                    MixedHopView::Cl(prepared) => Some(prepared.int_seq.as_ref()),
                    MixedHopView::V2(_) => None,
                })
                .collect();
            let hop_order: Vec<bool> = hops
                .iter()
                .map(|h| matches!(h, MixedHopView::V2(_)))
                .collect();
            let fp = walk_mixed_path_fingerprint(&v2_hops, &cl_sequences, &hop_order);
            if let Some(outcome) = memo_replay(memo, fp) {
                return outcome;
            }
            commit = Some((memo, fp));
        }
    }

    // Hop assembly, after the memo probe: each view becomes its `PieceView`
    // (V2 borrows the state; CL O(1)-clones the projection's `Arc` tables, so
    // nothing is derived or rebuilt per solve).
    let pieces: Vec<PieceView> = hops.iter().map(|h| h.piece_view()).collect();

    let outcome = solve_active_set_path(&pieces, cfg, env);
    if let Some((memo, fp)) = commit {
        memo_commit(memo, fp, &outcome);
    }
    outcome
}
