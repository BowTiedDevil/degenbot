use degenbot_math::v2::IntHopState;

use super::active_set::{solve_active_set_path, PieceView, WalkOutcome};
use super::crossings::{
    build_cl_crossing_table, build_cl_word_profiles_from_crossings, build_word_profiles,
    cl_walk_hop, cl_walk_hop_cached,
};
use super::memo::{walk_mixed_path_fingerprint, walk_path_fingerprint, MemoProbe, WalkMemo};
use super::{ClCrossingTable, ClProfileTable, IntV3TickRangeSequence};
use crate::profit_envelope::PathBoundLines;
use crate::runtime::SolveRuntimeConfig;

/// One CL hop's prepared tables (crossing + word profiles): the mixed solve
/// entry's per-CL-hop intake — built ONCE per (pool, direction) by the
/// projection, or derived via [`ClSolveTables::derive`] by tableless callers
/// (their cost). The all-CL entry takes [`PreparedClHop`] directly; this type
/// rides only until the mixed side's own consolidation.
#[derive(Clone)]
pub struct ClSolveTables {
    pub crossings: std::sync::Arc<ClCrossingTable>,
    pub profiles: std::sync::Arc<ClProfileTable>,
    /// [`walk_path_fingerprint`] of the sequence these tables were derived
    /// from, recorded at derivation and forwarded by
    /// [`PreparedClHop::to_solve_tables`]. The per-hop pairing gate that once
    /// consumed it is gone (the all-CL pairing is structural now); the field
    /// rides with the type until the mixed side consolidates.
    pub source_fingerprint: u128,
}

impl ClSolveTables {
    #[must_use]
    pub fn derive(seq: &IntV3TickRangeSequence) -> Self {
        let crossings = std::sync::Arc::new(build_cl_crossing_table(seq));
        let profiles = std::sync::Arc::new(build_word_profiles(&crossings));
        Self {
            crossings,
            profiles,
            source_fingerprint: walk_path_fingerprint(&[seq]),
        }
    }
}

/// One CL hop's DERIVATION value: the tick-range sequence it was derived from,
/// paired with the crossing table + word profiles built from that sequence and
/// the sequence's derivation fingerprint, folded ONCE per (pool, direction) by
/// the projection.
///
/// This is the projection-carried shape [`ResolvedHop::V3`/`V4`] carries (see
/// `mixed::ResolvedHop`) AND the all-CL solve entry's intake: the sequence and
/// its tables travel as one value, so the pairing the deleted gate defended
/// is structural. The mixed entry still takes [`ClSolveTables`] —
/// [`PreparedClHop::to_solve_tables`] translates at that call site. Folding
/// `source_fingerprint` HERE, not at solve time, is the point of the value: the
/// fingerprint is a pure function of the sequence, so it is derived once at
/// derivation and stored.
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
    /// [`walk_path_fingerprint`] of `int_seq`, recorded at derivation (the
    /// all-CL entry's memo key folds the hops' sequences directly; this field
    /// is the mixed-side translate's payload).
    pub source_fingerprint: u128,
}

impl PreparedClHop {
    /// Derive the crossing table + word profiles from `int_seq` and fold the
    /// sequence's derivation fingerprint once — the projection's construction.
    ///
    /// The crossing table is built directly and the word profiles derived from
    /// that same table (not rebuilt from the sequence), so the O(n²) crossing
    /// build runs exactly once per (pool, direction).
    #[must_use]
    pub fn derive(int_seq: std::sync::Arc<IntV3TickRangeSequence>) -> Self {
        let crossings = std::sync::Arc::new(build_cl_crossing_table(&int_seq));
        let profiles = std::sync::Arc::new(build_cl_word_profiles_from_crossings(&crossings));
        let source_fingerprint = walk_path_fingerprint(&[int_seq.as_ref()]);
        Self {
            int_seq,
            crossings,
            profiles,
            source_fingerprint,
        }
    }

    /// Wrap an already-built sequence + tables (offline/replay callers that
    /// build the tables themselves), folding the fingerprint once.
    #[must_use]
    pub fn from_parts(
        int_seq: std::sync::Arc<IntV3TickRangeSequence>,
        crossings: std::sync::Arc<ClCrossingTable>,
        profiles: std::sync::Arc<ClProfileTable>,
    ) -> Self {
        let source_fingerprint = walk_path_fingerprint(&[int_seq.as_ref()]);
        Self {
            int_seq,
            crossings,
            profiles,
            source_fingerprint,
        }
    }

    /// Translate to the solve entries' intake type: `Arc`-clone the tables and
    /// forward the derivation-time fingerprint. The entries' signatures (and
    /// the tables themselves) are unchanged.
    #[must_use]
    pub fn to_solve_tables(&self) -> ClSolveTables {
        ClSolveTables {
            crossings: std::sync::Arc::clone(&self.crossings),
            profiles: std::sync::Arc::clone(&self.profiles),
            source_fingerprint: self.source_fingerprint,
        }
    }
}

/// Build one CL hop's walk view from a sequence and its optional prepared
/// tables. `Some` O(1)-clones the projection's `Arc` tables; `None` builds
/// them here. The mixed entry assembles its CL hops through this seam.
fn cl_hop_view<'a>(
    seq: &'a IntV3TickRangeSequence,
    prepared: Option<&ClSolveTables>,
) -> PieceView<'a> {
    match prepared {
        Some(p) => cl_walk_hop_cached(seq, Some(&p.crossings), Some(&p.profiles)),
        None => cl_walk_hop(seq, None),
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
/// - `v2_hops[i]`: V2 hop state at position `i` (`None` for CL positions)
/// - `cl_sequences[i]`: CL tick-range sequence at position `i` (`None` for V2 positions)
/// - `cl_prepared[i]`: cached projection tables (`None` for V2 positions;
///   `None` derives them per call at the caller's cost for offline callers)
/// - `hop_order`: true = V2 hop, false = CL hop
///
/// Returns `(optimal_input, profit, hop_outputs)` or `None` if not profitable.
/// THE mixed V2+CL solve entry — prepared tables ride [`ClSolveTables`] per CL
/// hop position; `None` derives them at the caller's cost, the
/// offline/replay shape. `memo` (mirroring [`solve_cl_piecewise`]
/// position-for-position) is the caller's engine-owned cross-block
/// composition memo: the entry probes by [`walk_mixed_path_fingerprint`]
/// BEFORE hop assembly — the key needs only the hop states, sequences, and
/// order, so a cached `Hit` replays the solution and a cached `Negative`
/// skips the walk, `note_cost`, and store without paying for assembly; a
/// `Miss` falls through to hop assembly + solve + `note_cost` + `store`.
/// Pure-V2 compositions (no CL hops at all) route through this entry from
/// `mixed::solve` too, so they gain memo coverage here as well. (The
/// per-CL-hop table source gate this entry once ran is gone: the pairing it
/// defended is structural on the all-CL side, and the mixed side consolidates
/// with [`ClSolveTables`] in a later chunk.)
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
    memo: Option<&WalkMemo>,
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

    // Cheap slot validation stays ahead of the memo probe: every position
    // must carry its hop. (No assembly here — the `PieceView` builds stay
    // below the probe, so replays never pay for them.)
    for (i, &is_v2) in hop_order.iter().enumerate() {
        if is_v2 {
            if v2_hops[i].is_none() {
                return WalkOutcome::none();
            }
        } else if cl_sequences[i].is_none() {
            return WalkOutcome::none();
        }
    }

    // Cross-block composition memo: the mixed fingerprint folds every
    // position's family tag + hop state (V2: the full `IntHopState` the
    // constant-product view consumes; CL: the same per-range folding the
    // all-CL key uses), so the key is exact for mixed compositions — cfg is
    // fixed per engine lifetime and env (PathBoundLines) is a pure
    // derivation of hop state + cfg. The probe runs BEFORE hop assembly — the
    // key needs only the hop states, sequences, and order, so a `Hit` or
    // `Negative` returns without paying the `PieceView` builds. `None` memo =
    // disabled run never pays the fingerprint or the lock.
    let mut commit = None;
    if let Some(memo) = memo {
        if memo.active() {
            let fp = walk_mixed_path_fingerprint(v2_hops, cl_sequences, hop_order);
            if let Some(outcome) = memo_replay(memo, fp) {
                return outcome;
            }
            commit = Some((memo, fp));
        }
    }

    // Hop assembly, after the memo probe. The per-CL-hop table source gate
    // that once ran here is gone: the projection hands each hop a prepared
    // value paired with its own sequence, so positional pairing is structural.
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

    let outcome = solve_active_set_path(&hops, cfg, env);
    if let Some((memo, fp)) = commit {
        memo_commit(memo, fp, &outcome);
    }
    outcome
}
