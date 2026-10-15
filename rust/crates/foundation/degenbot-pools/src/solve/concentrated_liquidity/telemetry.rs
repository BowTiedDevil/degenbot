//! Walk telemetry: the event census the per-solve tally carries, plus the
//! timed-section helper the walk uses to attribute its wall time.
//!
//! Nothing here is ambient any more. The active-set walk, the crossing helpers,
//! and the word-profile build all thread their own owned accumulator
//! (`concentrated_liquidity::tally`) into every writer, so the event census lives in that
//! accumulator and its increment still carries the runtime census gate.
//! Compiling `degenbot-pools` without its `telemetry` feature (the default)
//! flushes every writer to a no-op, so the counters read as zero and the release
//! build pays nothing for them.

use alloy::primitives::U256;

use super::active_set::PieceView;
#[cfg(feature = "telemetry")]
use super::active_set::{landed_any_above, simulate_walk_path};
#[cfg(feature = "telemetry")]
use super::crossings::walk_event_first_above_predicted;
use super::tally::{WalkSection, WalkTally};
use crate::solve::runtime::SolveRuntimeConfig;

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
    /// `const`-constructible zero tally.
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
    match (x_r, pred) {
        (None, None) => tally.census_mut().terminal_agree += 1,
        (None, Some(_)) => tally.census_mut().terminal_disagree += 1,
        (Some(_), None) => tally.census_mut().pred_none += 1,
        (Some(lo), Some(pa)) => {
            if pa > lo && pa <= hi {
                // Verify probes: pa crosses OUT of the tuple, and pa−1 does
                // not — which proves pa is the exact first-above.
                let above = landed_any_above(&simulate_walk_path(pa, hops, tally).landed, ks);
                let below_ok = pa.is_zero()
                    || !landed_any_above(
                        &simulate_walk_path(pa - U256::ONE, hops, tally).landed,
                        ks,
                    );
                let c = tally.census_mut();
                c.pieces += 1;
                c.in_bracket += 1;
                c.census_sims += 2;
                if above && below_ok {
                    c.exact += 1;
                }
            } else if pa <= lo {
                let b = event_census_bucket(lo + U256::ONE - pa);
                let c = tally.census_mut();
                c.pieces += 1;
                c.early[b] += 1;
            } else {
                let b = event_census_bucket(pa - hi);
                let c = tally.census_mut();
                c.pieces += 1;
                c.late[b] += 1;
            }
        }
    }
    if let Some(lo) = x_r {
        tally.record_census_pieces(ks, lo);
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
