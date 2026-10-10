//! Ambient walk telemetry: the event census the per-solve tally has not yet
//! absorbed, plus the process-wide timing statics the cache lab still drains.
//!
//! The active-set walk, the crossing helpers, and the word-profile build all
//! thread their own owned accumulator (`cl::tally`) now; what remains here is
//! the event census alone — its increment carries the runtime census gate. The
//! process-wide timing statics survive for the cache lab's swap-sweep readout
//! until that reader cuts over to the collector's drain. Compiling
//! `degenbot-solvers` without its `telemetry` feature (the default) flushes
//! every writer to a no-op, so the counters read as zero and the release build
//! pays nothing for them.

use alloy::primitives::U256;

#[cfg(feature = "telemetry")]
use super::active_set::{landed_any_above, simulate_walk_path};
use super::active_set::{PieceView, WalkStats};
#[cfg(feature = "telemetry")]
use super::crossings::walk_event_first_above_predicted;
use super::tally::{WalkSection, WalkTally};
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
    hops: &[PieceView],
    ks: &[usize],
    x_r: Option<U256>,
    hi: U256,
    cfg: &SolveRuntimeConfig,
    tally: &mut WalkTally,
) {
    #[cfg(feature = "telemetry")]
    event_census_record_inner(hops, ks, x_r, hi, cfg, tally);
    #[cfg(not(feature = "telemetry"))]
    let _ = (hops, ks, x_r, hi, cfg, tally);
}

#[cfg(feature = "telemetry")]
fn event_census_record_inner(
    hops: &[PieceView],
    ks: &[usize],
    x_r: Option<U256>,
    hi: U256,
    cfg: &SolveRuntimeConfig,
    tally: &mut WalkTally,
) {
    if !event_census_on(cfg) {
        return;
    }
    let pred = walk_event_first_above_predicted(hops, ks, tally);
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
                let above = landed_any_above(&simulate_walk_path(pa, hops, tally).landed, ks);
                let below_ok = pa.is_zero()
                    || !landed_any_above(
                        &simulate_walk_path(pa - U256::ONE, hops, tally).landed,
                        ks,
                    );
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
    /// Snapshots the section start instant plus the accumulated sims and
    /// walk-sim wall time the section delta is measured against.
    #[inline]
    pub(super) fn start(tally: &WalkTally) -> Self {
        #[cfg(feature = "telemetry")]
        {
            Self {
                at: std::time::Instant::now(),
                probes: u64::try_from(tally.sims()).unwrap_or(u64::MAX),
                probe_ns: tally.sim_ns(),
            }
        }
        #[cfg(not(feature = "telemetry"))]
        {
            let _ = tally;
            Self {}
        }
    }

    /// Commits the section's `(wall, sims, sim-ns)` triple into the solve's
    /// per-solve accumulator.
    #[inline]
    pub(super) fn commit(self, tally: &mut WalkTally, section: WalkSection) {
        #[cfg(not(feature = "telemetry"))]
        let _ = (self, tally, section);
        #[cfg(feature = "telemetry")]
        {
            let wall = u64::try_from(self.at.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let now_probes = u64::try_from(tally.sims()).unwrap_or(u64::MAX);
            let sims = now_probes.saturating_sub(self.probes);
            let sim_ns = tally.sim_ns().saturating_sub(self.probe_ns);
            tally.mark_commit(section, wall, sims, sim_ns);
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

/// Drain the ambient per-thread census state on the calling thread.
/// Drain = read-and-clear; the discarded value is the prior path's.
pub(crate) fn reset_walk_stats() {
    #[cfg(feature = "telemetry")]
    reset_walk_stats_inner();
}

#[cfg(feature = "telemetry")]
fn reset_walk_stats_inner() {
    EVENT_CENSUS.with(|c| c.set(WalkEventCensus::default()));
}

/// Snapshot (no clearing) of the remaining ambient per-thread walk telemetry:
/// the event census, which the solve entry folds into its outcome.
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
        census: EVENT_CENSUS.with(std::cell::Cell::get),
        ..WalkStats::default()
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
