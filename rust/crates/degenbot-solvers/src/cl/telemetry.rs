//! Ambient walk telemetry: per-thread walk-combinator counters (pieces, sims,
//! word steps, refinement and per-edge probe splits), the event census, and the
//! process-wide timing statics the cache lab reads.
//!
//! Compiling `degenbot-solvers` without its `telemetry` feature (the default)
//! flushes every writer to a no-op, so the counters read as zero and the
//! release build pays nothing for them. The read accessors keep their shape.

use alloy::primitives::U256;

#[cfg(feature = "telemetry")]
use super::active_set::{landed_any_above, simulate_walk_path};
use super::active_set::{WalkHop, WalkStats};
#[cfg(feature = "telemetry")]
use super::crossings::walk_event_first_above_predicted;
use crate::runtime::SolveRuntimeConfig;

// ---------------------------------------------------------------------------
// Event census (loop 15): predicted vs bisected first-above
// ---------------------------------------------------------------------------

/// One replay session's census tally of the nested inversion against the
/// bisection ground truth (bracket `[lo+1, hi]` from the seeded search).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkEventCensus {
    /// Bounded pieces the census observed.
    pub pieces: u64,
    /// Predicted first-above verified EXACT (the two verify probes prove both
    /// sides of the crossing).
    pub exact: u64,
    /// Prediction inside the bisection bracket but not probe-exact.
    pub in_bracket: u64,
    /// Prediction earlier than the bracket (`pred <= lo`).
    pub early: [u64; 4],
    /// Prediction later than the bracket (`pred > hi`).
    pub late: [u64; 4],
    /// Bracketed piece but no prediction (the loop-14 composed-model
    /// phenomenon — 232/233 there).
    pub pred_none: u64,
    /// Terminal pieces where both agree the region is unbounded.
    pub terminal_agree: u64,
    /// Terminal piece where the prediction claims a bound.
    pub terminal_disagree: u64,
    /// Verify sims the census itself spent (transparency).
    pub census_sims: u64,
}

impl WalkEventCensus {
    /// `const`-constructible zero tally (thread-local initializer).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pieces: 0,
            exact: 0,
            in_bracket: 0,
            early: [0; 4],
            late: [0; 4],
            pred_none: 0,
            terminal_agree: 0,
            terminal_disagree: 0,
            census_sims: 0,
        }
    }
}

thread_local! {
    static EVENT_CENSUS: std::cell::Cell<WalkEventCensus> =
        const { std::cell::Cell::new(WalkEventCensus::new()) };
    // (ks, right-edge) per bounded piece — the T2 cross-block edge-shift
    // recorder.
    pub(super) static EVENT_CENSUS_PIECES: std::cell::RefCell<Vec<(Vec<usize>, U256)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Census gate from the injected runtime config (T4: no env read here —
/// the owner packs the stance at construction).
#[cfg(feature = "telemetry")]
fn event_census_on(cfg: &SolveRuntimeConfig) -> bool {
    cfg.walk_event_census
}

#[cfg(feature = "telemetry")]
fn event_census_bucket(d: U256) -> usize {
    if d <= U256::from(4u64) {
        0
    } else if d <= U256::from(65_536u64) {
        1
    } else if d <= (U256::ONE << 40) {
        2
    } else {
        3
    }
}

/// Record one piece: `x_r = Some(lo)` with `hi` above (bracket `[lo+1, hi]`)
/// when bounded, `None` for a terminal piece.
pub(super) fn event_census_record(
    hops: &[WalkHop],
    ks: &[usize],
    x_r: Option<U256>,
    hi: U256,
    cfg: &SolveRuntimeConfig,
) {
    #[cfg(feature = "telemetry")]
    event_census_record_inner(hops, ks, x_r, hi, cfg);
    #[cfg(not(feature = "telemetry"))]
    let _ = (hops, ks, x_r, hi, cfg);
}

#[cfg(feature = "telemetry")]
fn event_census_record_inner(
    hops: &[WalkHop],
    ks: &[usize],
    x_r: Option<U256>,
    hi: U256,
    cfg: &SolveRuntimeConfig,
) {
    if !event_census_on(cfg) {
        return;
    }
    let pred = walk_event_first_above_predicted(hops, ks);
    let mut c = EVENT_CENSUS.get();
    match (x_r, pred) {
        (None, None) => c.terminal_agree += 1,
        (None, Some(_)) => c.terminal_disagree += 1,
        (Some(_), None) => c.pred_none += 1,
        (Some(lo), Some(pa)) => {
            if pa > lo && pa <= hi {
                c.pieces += 1;
                c.in_bracket += 1;
                // Verify probes: pa crosses OUT of the tuple, and pa−1 does
                // not — which proves pa is the exact first-above.
                let above = landed_any_above(&simulate_walk_path(pa, hops).landed, ks);
                let below_ok = pa.is_zero()
                    || !landed_any_above(&simulate_walk_path(pa - U256::ONE, hops).landed, ks);
                c.census_sims += 2;
                if above && below_ok {
                    c.exact += 1;
                }
            } else if pa <= lo {
                c.pieces += 1;
                let b = event_census_bucket(lo + U256::ONE - pa);
                c.early[b] += 1;
            } else {
                c.pieces += 1;
                let b = event_census_bucket(pa - hi);
                c.late[b] += 1;
            }
        }
    }
    EVENT_CENSUS.set(c);
    if let Some(lo) = x_r {
        EVENT_CENSUS_PIECES.with_borrow_mut(|p| p.push((ks.to_vec(), lo)));
    }
}

/// Pointwise-accumulate another tally (the replay example's grand totals).
impl WalkEventCensus {
    pub fn accumulate_event_census(&mut self, other: WalkEventCensus) {
        self.pieces += other.pieces;
        self.exact += other.exact;
        self.in_bracket += other.in_bracket;
        for i in 0..4 {
            self.early[i] += other.early[i];
            self.late[i] += other.late[i];
        }
        self.pred_none += other.pred_none;
        self.terminal_agree += other.terminal_agree;
        self.terminal_disagree += other.terminal_disagree;
        self.census_sims += other.census_sims;
    }
}

/// Largest ending-range index whose crossing is affordable with `available`
/// gross input.
///
/// `crossing_gross_input` is non-decreasing in `k` (it is a prefix sum of
/// non-negative per-range gross inputs), so the landed index is a partition
/// point. Ties (zero-liquidity ranges cost nothing to cross) resolve to the
/// LARGEST index — the swap entered every zero-cost range.
/// Loop-17 census: total wall time spent inside `simulate_walk_path`
/// (process-wide atomics — avoids thread-local TLS budget).
pub static WALK_SIM_NS_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: total anchor computation wall time (per-piece shifted
/// Möbius + isqrt).
pub static WALK_ANCHOR_NS_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: event-solver prediction wall time.
pub static WALK_PRED_NS_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: whole active-set walk wall time.
pub static WALK_SOLVE_NS_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: left-edge determination (wall ns + sims consumed inside).
pub static WALK_CENSUS_EDGE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_EDGE_SIMS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: per-section walk-sim wall time (ns) — subtract from the
/// section wall to isolate non-sim machinery.
pub static WALK_CENSUS_SIMNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_EDGE_SIMNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: right-edge determination (wall ns + sims consumed inside).
pub static WALK_CENSUS_REDGE_NS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_REDGE_SIMS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_REDGE_SIMNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: direction test + advancement (wall ns + sims consumed).
pub static WALK_CENSUS_DIR_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_DIR_SIMS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_DIR_SIMNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 census: terminal refine (wall ns + sims consumed). Includes the
/// anchor-corner probes and the single-piece refine path.
pub static WALK_CENSUS_REFINE_NS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_REFINE_SIMS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_CENSUS_REFINE_SIMNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Loop-17 section census helper: records (wall ns, sims, sim wall ns)
/// between `Mark::start()` and `commit`, attributing them to a section.
pub(super) struct Mark {
    #[cfg(feature = "telemetry")]
    at: std::time::Instant,
    #[cfg(feature = "telemetry")]
    probes: u64,
    #[cfg(feature = "telemetry")]
    probe_ns: u64,
}

impl Mark {
    #[inline]
    pub(super) fn start() -> Self {
        #[cfg(feature = "telemetry")]
        {
            Self {
                at: std::time::Instant::now(),
                probes: u64::try_from(WALK_PATH_SIMULATIONS.with(std::cell::Cell::get))
                    .unwrap_or(u64::MAX),
                probe_ns: WALK_SIM_NS_TOTAL.load(std::sync::atomic::Ordering::Relaxed),
            }
        }
        #[cfg(not(feature = "telemetry"))]
        {
            Self {}
        }
    }

    #[inline]
    pub(super) fn commit(
        self,
        wall: &std::sync::atomic::AtomicU64,
        tally: &std::sync::atomic::AtomicU64,
        probe_ns: &std::sync::atomic::AtomicU64,
    ) {
        #[cfg(not(feature = "telemetry"))]
        let _ = (self, wall, tally, probe_ns);
        #[cfg(feature = "telemetry")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            wall.fetch_add(
                u64::try_from(self.at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                Relaxed,
            );
            let now_probes =
                u64::try_from(WALK_PATH_SIMULATIONS.with(std::cell::Cell::get)).unwrap_or(u64::MAX);
            tally.fetch_add(now_probes.saturating_sub(self.probes), Relaxed);
            probe_ns.fetch_add(
                WALK_SIM_NS_TOTAL
                    .load(Relaxed)
                    .saturating_sub(self.probe_ns),
                Relaxed,
            );
        }
    }
}

/// Loop-17 census: anchor-phase wall split — hop construction vs coefficient
/// compose vs model argmax (all ns, process-wide).
pub static WALK_ANCHOR_BUILD_NS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_ANCHOR_COMPOSE_NS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static WALK_ANCHOR_ARGMAX_NS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

// Test-only instrumentation for the active-set walk: pieces visited and
// path simulations executed per solve. Guard tests bound both (regression
// net against re-introducing combinatorial behavior).
//
// Thread-local because `cargo test` runs tests (and their solves) on
// separate threads concurrently — a shared static would mix counts.
thread_local! {
    // Production-scoped walk-combinator counters (see `WALK_STATS_SCOPE`).
    // Always-on: the rayon solve resets + reads them once per path to name the
    // cost driver of slow solves (pieces × simulations × word-boundary walk).
    pub(crate) static WALK_PIECES_VISITED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // See `WALK_PIECES_VISITED`.
    pub(crate) static WALK_PATH_SIMULATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Total `compute_swap_step_v3` steps executed inside `simulate_v3_range_swap`
    //'s word-boundary walk — the per-simulation cost driver for dense
    // (many-word-boundary) CL ranges. `sims × per-sim steps` is the real cost.
    pub(crate) static WALK_WORD_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Stop-time refinement (`walk_refine_window` ternary + dense sweep) sim
    // count — the measurement split for the 64-wei refinement-resolution cost.
    pub(crate) static WALK_REFINE_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Refinement split: sims in the ternary narrowing phase vs sims
    // in the final coarse-grid / dense-sweep phase. Names the probe-budget
    // driver so the next optimization touches the right loop.
    pub(crate) static WALK_TERNARY_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static WALK_GRID_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Loop-13 YHR3ZH atomization: per-piece window-edge bisection probes and
    // the transitional-anchor sweep. Everything else (straddle probes,
    // landed_beyond scans, skipped-tuple checks, neighbor coarse grids) is
    // the residual of total - (left+right+anchor+refine).
    pub(crate) static WALK_LEFT_EDGE_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static WALK_RIGHT_EDGE_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static WALK_ANCHOR_SIMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Event solver (loop 15): pieces whose right edge came from the
    // nested ceil-inversion (accepted on the two verify probes) vs pieces
    // that fell back to the grow + bisection. Live-corpus census: 158,283 of
    // 158,283 exact - the fallback is defense-in-depth, not a hot path.
    pub(crate) static WALK_EVENT_SOLVER_OK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static WALK_EVENT_SOLVER_FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Q3 telemetry: the largest word-boundary count any range reached on this
    // thread. DB audit (correct metric = max inter-init-tick gap in words,
    // per-pool ts): 210/47,679 registered UNI V3 pools have a >=128-word gap,
    // 161 fall in the solve window (<16 positions), and 27 have their current
    // tick inside one — so dense is load-bearing on real sparse pools today.
    // This observes the largest count, and a one-shot >= DENSE_OBSERVE_THRESHOLD
    // alert fires when a range approaches the profile threshold.
    pub(crate) static WALK_MAX_DENSE_WORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Drain ALL walk counters + census state on the calling thread.
/// Drain = read-and-clear; the discarded value is the prior path's.
pub(crate) fn reset_walk_stats() {
    #[cfg(feature = "telemetry")]
    reset_walk_stats_inner();
}

#[cfg(feature = "telemetry")]
fn reset_walk_stats_inner() {
    EVENT_CENSUS.with(|c| c.set(WalkEventCensus::default()));
    WALK_PIECES_VISITED.with(|c| c.set(0));
    WALK_PATH_SIMULATIONS.with(|c| c.set(0));
    WALK_WORD_STEPS.with(|c| c.set(0));
    WALK_REFINE_SIMS.with(|c| c.set(0));
    WALK_TERNARY_SIMS.with(|c| c.set(0));
    WALK_GRID_SIMS.with(|c| c.set(0));
    WALK_LEFT_EDGE_SIMS.with(|c| c.set(0));
    WALK_RIGHT_EDGE_SIMS.with(|c| c.set(0));
    WALK_ANCHOR_SIMS.with(|c| c.set(0));
    WALK_EVENT_SOLVER_OK.with(|c| c.set(0));
    WALK_EVENT_SOLVER_FALLBACKS.with(|c| c.set(0));
}

/// Snapshot (no clearing) of the full per-thread walk telemetry.
#[must_use]
pub(super) fn peek_walk_stats() -> WalkStats {
    #[cfg(feature = "telemetry")]
    {
        peek_walk_stats_inner()
    }
    #[cfg(not(feature = "telemetry"))]
    {
        WalkStats::default()
    }
}

#[cfg(feature = "telemetry")]
fn peek_walk_stats_inner() -> WalkStats {
    WalkStats {
        pieces: WALK_PIECES_VISITED.with(std::cell::Cell::get),
        sims: WALK_PATH_SIMULATIONS.with(std::cell::Cell::get),
        word_steps: WALK_WORD_STEPS.with(std::cell::Cell::get),
        refine_sims: WALK_REFINE_SIMS.with(std::cell::Cell::get),
        ternary_sims: WALK_TERNARY_SIMS.with(std::cell::Cell::get),
        grid_sims: WALK_GRID_SIMS.with(std::cell::Cell::get),
        left_edge_sims: WALK_LEFT_EDGE_SIMS.with(std::cell::Cell::get),
        right_edge_sims: WALK_RIGHT_EDGE_SIMS.with(std::cell::Cell::get),
        anchor_sims: WALK_ANCHOR_SIMS.with(std::cell::Cell::get),
        event_solver_ok: WALK_EVENT_SOLVER_OK.with(std::cell::Cell::get),
        event_solver_fallbacks: WALK_EVENT_SOLVER_FALLBACKS.with(std::cell::Cell::get),
        max_dense_words: WALK_MAX_DENSE_WORDS.with(std::cell::Cell::get),
        census: EVENT_CENSUS.with(std::cell::Cell::get),
    }
}

// ---------------------------------------------------------------------------
// Gated writer accessors
//
// Every ambient mutation routes through one of these helpers. With the
// `telemetry` feature off each is a no-op, so the counters stay at their
// zero initializers and the release build never touches the cells or statics.
// ---------------------------------------------------------------------------

/// Adds `by` to one thread-local walk counter.
macro_rules! gated_cell_bump {
    ($name:ident, $cell:ident) => {
        #[inline]
        pub(super) fn $name(by: usize) {
            #[cfg(feature = "telemetry")]
            $cell.with(|c| c.set(c.get() + by));
            #[cfg(not(feature = "telemetry"))]
            let _ = by;
        }
    };
}

gated_cell_bump!(bump_path_simulations, WALK_PATH_SIMULATIONS);
gated_cell_bump!(bump_pieces_visited, WALK_PIECES_VISITED);
gated_cell_bump!(bump_word_steps, WALK_WORD_STEPS);
gated_cell_bump!(bump_refine_sims, WALK_REFINE_SIMS);
gated_cell_bump!(bump_ternary_sims, WALK_TERNARY_SIMS);
gated_cell_bump!(bump_grid_sims, WALK_GRID_SIMS);
gated_cell_bump!(bump_left_edge_sims, WALK_LEFT_EDGE_SIMS);
gated_cell_bump!(bump_right_edge_sims, WALK_RIGHT_EDGE_SIMS);
gated_cell_bump!(bump_anchor_sims, WALK_ANCHOR_SIMS);
gated_cell_bump!(bump_event_solver_ok, WALK_EVENT_SOLVER_OK);
gated_cell_bump!(bump_event_solver_fallbacks, WALK_EVENT_SOLVER_FALLBACKS);

/// Adds `ns` to one process-wide timing counter.
macro_rules! gated_atomic_add {
    ($name:ident, $atomic:ident) => {
        #[inline]
        pub(super) fn $name(ns: u64) {
            #[cfg(feature = "telemetry")]
            $atomic.fetch_add(ns, std::sync::atomic::Ordering::Relaxed);
            #[cfg(not(feature = "telemetry"))]
            let _ = ns;
        }
    };
}

gated_atomic_add!(add_sim_ns, WALK_SIM_NS_TOTAL);
gated_atomic_add!(add_anchor_ns, WALK_ANCHOR_NS_TOTAL);
gated_atomic_add!(add_pred_ns, WALK_PRED_NS_TOTAL);
gated_atomic_add!(add_solve_ns, WALK_SOLVE_NS_TOTAL);
gated_atomic_add!(add_anchor_build_ns, WALK_ANCHOR_BUILD_NS);
gated_atomic_add!(add_anchor_compose_ns, WALK_ANCHOR_COMPOSE_NS);
gated_atomic_add!(add_anchor_argmax_ns, WALK_ANCHOR_ARGMAX_NS);

/// Raises this thread's largest observed word-boundary count to at least `n`.
#[inline]
pub(super) fn observe_max_dense_words(n: usize) {
    #[cfg(feature = "telemetry")]
    WALK_MAX_DENSE_WORDS.with(|m| {
        if n > m.get() {
            m.set(n);
        }
    });
    #[cfg(not(feature = "telemetry"))]
    let _ = n;
}

/// Zeroes this thread's pieces/sims at the start of a solve.
#[inline]
pub(super) fn clear_walk_pieces_and_sims() {
    #[cfg(feature = "telemetry")]
    {
        WALK_PIECES_VISITED.with(|c| c.set(0));
        WALK_PATH_SIMULATIONS.with(|c| c.set(0));
    }
}

/// Sims executed so far on this thread (`0` without `telemetry`).
#[inline]
pub(super) fn path_simulations_now() -> usize {
    #[cfg(feature = "telemetry")]
    {
        WALK_PATH_SIMULATIONS.with(std::cell::Cell::get)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        0
    }
}

/// Drains the thread's census piece recorder (empty without `telemetry`).
#[inline]
pub(super) fn take_event_census_pieces() -> Vec<(Vec<usize>, U256)> {
    #[cfg(feature = "telemetry")]
    {
        EVENT_CENSUS_PIECES.with_borrow_mut(std::mem::take)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        Vec::new()
    }
}
