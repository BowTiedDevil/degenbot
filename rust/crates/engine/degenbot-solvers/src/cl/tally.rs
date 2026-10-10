//! The owned walk-telemetry value: the per-solve accumulator the walk threads
//! by `&mut`, and the engine-owned collector it merges into once per solve.
//!
//! These are the owned successors to the ambient counters in `cl/telemetry.rs`:
//! a solve-scoped stack value replaces the thread-local cells, and one
//! `parking_lot`-guarded collector under an `Arc` replaces the process-global
//! timing statics. The collector type is deliberately not feature-gated — it
//! exists, and its signatures stay single, in every build; only the
//! accumulator's writer bodies ride the crate's default-off `telemetry`
//! feature, so a default build's bumps compile to no-ops and every value reads
//! as zero.

use alloy::primitives::U256;

use super::active_set::WalkStats;
use super::telemetry::WalkEventCensus;

// ---------------------------------------------------------------------------
// WalkNsCensus — the process-cumulative wall-time census
// ---------------------------------------------------------------------------

/// Process-cumulative walk wall-time census, in nanoseconds.
///
/// The owned successor to the process-global timing statics: nineteen `u64`
/// counters, split into the whole-solve entry timer, three phase timers
/// (sim / anchor / prediction), and the four `(wall, sims, sim-ns)` section
/// triples the section helper commits. Merge adds; [`WalkTelemetry::drain_ns`]
/// swaps the whole census back to zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkNsCensus {
    /// Wall time inside full-path simulations.
    pub sim: u64,
    /// Wall time in per-piece anchor computation.
    pub anchor: u64,
    /// Wall time in event-solver prediction.
    pub pred: u64,
    /// Wall time of the whole active-set walk entry.
    pub solve: u64,
    /// Left-window-edge determination wall time.
    pub edge_wall: u64,
    /// Sims consumed inside the left-edge section.
    pub edge_sims: u64,
    /// Walk-sim wall time inside the left-edge section.
    pub edge_simns: u64,
    /// Right-window-edge determination wall time.
    pub redge_wall: u64,
    /// Sims consumed inside the right-edge section.
    pub redge_sims: u64,
    /// Walk-sim wall time inside the right-edge section.
    pub redge_simns: u64,
    /// Direction test + advancement wall time.
    pub dir_wall: u64,
    /// Sims consumed inside the direction section.
    pub dir_sims: u64,
    /// Walk-sim wall time inside the direction section.
    pub dir_simns: u64,
    /// Terminal-refinement wall time.
    pub refine_wall: u64,
    /// Sims consumed inside the refinement section.
    pub refine_sims: u64,
    /// Walk-sim wall time inside the refinement section.
    pub refine_simns: u64,
    /// Anchor hop-construction wall time.
    pub anchor_build: u64,
    /// Anchor coefficient-compose wall time.
    pub anchor_compose: u64,
    /// Anchor model-argmax wall time.
    pub anchor_argmax: u64,
}

impl WalkNsCensus {
    /// The all-zero census.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sim: 0,
            anchor: 0,
            pred: 0,
            solve: 0,
            edge_wall: 0,
            edge_sims: 0,
            edge_simns: 0,
            redge_wall: 0,
            redge_sims: 0,
            redge_simns: 0,
            dir_wall: 0,
            dir_sims: 0,
            dir_simns: 0,
            refine_wall: 0,
            refine_sims: 0,
            refine_simns: 0,
            anchor_build: 0,
            anchor_compose: 0,
            anchor_argmax: 0,
        }
    }

    /// Pointwise-add `other` into this census (the per-solve merge is the only
    /// caller; the whole adds are `u64` saturating at wrap, as the atomics did).
    pub fn accumulate(&mut self, other: Self) {
        self.sim += other.sim;
        self.anchor += other.anchor;
        self.pred += other.pred;
        self.solve += other.solve;
        self.edge_wall += other.edge_wall;
        self.edge_sims += other.edge_sims;
        self.edge_simns += other.edge_simns;
        self.redge_wall += other.redge_wall;
        self.redge_sims += other.redge_sims;
        self.redge_simns += other.redge_simns;
        self.dir_wall += other.dir_wall;
        self.dir_sims += other.dir_sims;
        self.dir_simns += other.dir_simns;
        self.refine_wall += other.refine_wall;
        self.refine_sims += other.refine_sims;
        self.refine_simns += other.refine_simns;
        self.anchor_build += other.anchor_build;
        self.anchor_compose += other.anchor_compose;
        self.anchor_argmax += other.anchor_argmax;
    }
}

// ---------------------------------------------------------------------------
// WalkSection — the timed section enum
// ---------------------------------------------------------------------------

/// One timed section of the active-set walk — the four section-helper commit
/// destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkSection {
    /// Left-window-edge determination.
    Edge,
    /// Right-window-edge determination.
    Redge,
    /// Direction test + advancement.
    Dir,
    /// Terminal refinement.
    Refine,
}

// ---------------------------------------------------------------------------
// WalkTally — the per-solve accumulator
// ---------------------------------------------------------------------------

/// The per-solve walk-telemetry accumulator.
///
/// One value per `solve_active_set_path` call, created on the solving stack and
/// threaded by `&mut` to every writer; merged into the engine-owned
/// [`WalkTelemetry`] once, at exit. Its counter fields are the [`WalkStats`]
/// projection `WalkOutcome` already carries; its census holds the per-solve
/// share of the wall-time timings.
///
/// Without the crate's `telemetry` feature every bump is a no-op, so the fields
/// stay zero and a default build records nothing. The value is solve-scoped, so
/// it can never mix counts across concurrent solves or tests — the property the
/// thread-local cells bought before it.
#[derive(Debug, Clone, Default)]
pub struct WalkTally {
    pieces: usize,
    sims: usize,
    word_steps: usize,
    refine_sims: usize,
    ternary_sims: usize,
    grid_sims: usize,
    left_edge_sims: usize,
    right_edge_sims: usize,
    anchor_sims: usize,
    event_solver_ok: usize,
    event_solver_fallbacks: usize,
    max_dense_words: usize,
    census: WalkEventCensus,
    ns: WalkNsCensus,
    census_pieces: Vec<(Vec<usize>, U256)>,
}

impl WalkTally {
    /// Adds `by` to one counter, or nothing without the `telemetry` feature.
    #[inline]
    fn bump(field: &mut usize, by: usize) {
        #[cfg(feature = "telemetry")]
        {
            *field += by;
        }
        #[cfg(not(feature = "telemetry"))]
        let _ = (field, by);
    }

    /// Adds `ns` to one census timer, or nothing without the `telemetry`
    /// feature.
    #[inline]
    fn add_ns(field: &mut u64, ns: u64) {
        #[cfg(feature = "telemetry")]
        {
            *field += ns;
        }
        #[cfg(not(feature = "telemetry"))]
        let _ = (field, ns);
    }

    /// Raises one max-field to at least `n`, or nothing without the
    /// `telemetry` feature.
    #[inline]
    fn observe(field: &mut usize, n: usize) {
        #[cfg(feature = "telemetry")]
        {
            if n > *field {
                *field = n;
            }
        }
        #[cfg(not(feature = "telemetry"))]
        let _ = (field, n);
    }

    /// Appends one census piece, or nothing without the `telemetry` feature.
    #[inline]
    fn push_census_piece(log: &mut Vec<(Vec<usize>, U256)>, ks: &[usize], lo: U256) {
        #[cfg(feature = "telemetry")]
        {
            log.push((ks.to_vec(), lo));
        }
        #[cfg(not(feature = "telemetry"))]
        let _ = (log, ks, lo);
    }

    /// Commits one timed section triple, or nothing without the `telemetry`
    /// feature.
    #[inline]
    fn commit_section(
        ns: &mut WalkNsCensus,
        section: WalkSection,
        wall_ns: u64,
        sims: u64,
        sim_ns: u64,
    ) {
        #[cfg(feature = "telemetry")]
        {
            match section {
                WalkSection::Edge => {
                    ns.edge_wall += wall_ns;
                    ns.edge_sims += sims;
                    ns.edge_simns += sim_ns;
                }
                WalkSection::Redge => {
                    ns.redge_wall += wall_ns;
                    ns.redge_sims += sims;
                    ns.redge_simns += sim_ns;
                }
                WalkSection::Dir => {
                    ns.dir_wall += wall_ns;
                    ns.dir_sims += sims;
                    ns.dir_simns += sim_ns;
                }
                WalkSection::Refine => {
                    ns.refine_wall += wall_ns;
                    ns.refine_sims += sims;
                    ns.refine_simns += sim_ns;
                }
            }
        }
        #[cfg(not(feature = "telemetry"))]
        let _ = (ns, section, wall_ns, sims, sim_ns);
    }

    /// Counts pieces visited by the monotone walk.
    #[inline]
    pub fn bump_pieces(&mut self, by: usize) {
        Self::bump(&mut self.pieces, by);
    }

    /// Counts full-path simulations (was `bump_path_simulations`).
    #[inline]
    pub fn bump_sims(&mut self, by: usize) {
        Self::bump(&mut self.sims, by);
    }

    /// Counts word-boundary steps inside every simulation.
    #[inline]
    pub fn bump_word_steps(&mut self, by: usize) {
        Self::bump(&mut self.word_steps, by);
    }

    /// Counts stop-time refinement probes.
    #[inline]
    pub fn bump_refine_sims(&mut self, by: usize) {
        Self::bump(&mut self.refine_sims, by);
    }

    /// Counts ternary-narrowing phase sims.
    #[inline]
    pub fn bump_ternary_sims(&mut self, by: usize) {
        Self::bump(&mut self.ternary_sims, by);
    }

    /// Counts final grid / dense-sweep phase sims.
    #[inline]
    pub fn bump_grid_sims(&mut self, by: usize) {
        Self::bump(&mut self.grid_sims, by);
    }

    /// Counts left-window-edge bisection/scan probes.
    #[inline]
    pub fn bump_left_edge_sims(&mut self, by: usize) {
        Self::bump(&mut self.left_edge_sims, by);
    }

    /// Counts right-window-edge (seeded) bisection probes.
    #[inline]
    pub fn bump_right_edge_sims(&mut self, by: usize) {
        Self::bump(&mut self.right_edge_sims, by);
    }

    /// Counts transitional-anchor sweep probes.
    #[inline]
    pub fn bump_anchor_sims(&mut self, by: usize) {
        Self::bump(&mut self.anchor_sims, by);
    }

    /// Counts event-solver pieces accepted on the verify probes.
    #[inline]
    pub fn bump_event_solver_ok(&mut self, by: usize) {
        Self::bump(&mut self.event_solver_ok, by);
    }

    /// Counts event-solver pieces that fell back to grow + bisection.
    #[inline]
    pub fn bump_event_solver_fallbacks(&mut self, by: usize) {
        Self::bump(&mut self.event_solver_fallbacks, by);
    }

    /// Raises the largest observed word-boundary count to at least `n`.
    #[inline]
    pub fn observe_max_dense_words(&mut self, n: usize) {
        Self::observe(&mut self.max_dense_words, n);
    }

    /// Adds wall time spent inside full-path simulations.
    #[inline]
    pub fn add_sim_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.sim, ns);
    }

    /// Adds anchor-computation wall time.
    #[inline]
    pub fn add_anchor_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.anchor, ns);
    }

    /// Adds event-solver prediction wall time.
    #[inline]
    pub fn add_pred_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.pred, ns);
    }

    /// Adds whole-active-set-walk wall time.
    #[inline]
    pub fn add_solve_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.solve, ns);
    }

    /// Adds anchor hop-construction wall time.
    #[inline]
    pub fn add_anchor_build_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.anchor_build, ns);
    }

    /// Adds anchor coefficient-compose wall time.
    #[inline]
    pub fn add_anchor_compose_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.anchor_compose, ns);
    }

    /// Adds anchor model-argmax wall time.
    #[inline]
    pub fn add_anchor_argmax_ns(&mut self, ns: u64) {
        Self::add_ns(&mut self.ns.anchor_argmax, ns);
    }

    /// Commits one `(wall, sims, sim-ns)` triple to its timed section.
    #[inline]
    pub fn mark_commit(&mut self, section: WalkSection, wall_ns: u64, sims: u64, sim_ns: u64) {
        Self::commit_section(&mut self.ns, section, wall_ns, sims, sim_ns);
    }

    /// Records one census piece (keys + right edge) for the collector log.
    #[inline]
    pub fn record_census_pieces(&mut self, ks: &[usize], lo: U256) {
        Self::push_census_piece(&mut self.census_pieces, ks, lo);
    }

    /// Mutable access to the event-census tally the section recorder fills.
    #[inline]
    pub fn census_mut(&mut self) -> &mut WalkEventCensus {
        &mut self.census
    }

    /// Takes the recorded census pieces, leaving the log empty.
    #[must_use]
    pub fn take_census_pieces(&mut self) -> Vec<(Vec<usize>, U256)> {
        std::mem::take(&mut self.census_pieces)
    }

    /// The [`WalkStats`] projection this tally carries — the value
    /// `WalkOutcome` reports.
    #[must_use]
    pub fn walk_stats(&self) -> WalkStats {
        WalkStats {
            pieces: self.pieces,
            sims: self.sims,
            word_steps: self.word_steps,
            refine_sims: self.refine_sims,
            ternary_sims: self.ternary_sims,
            grid_sims: self.grid_sims,
            left_edge_sims: self.left_edge_sims,
            right_edge_sims: self.right_edge_sims,
            anchor_sims: self.anchor_sims,
            event_solver_ok: self.event_solver_ok,
            event_solver_fallbacks: self.event_solver_fallbacks,
            max_dense_words: self.max_dense_words,
            census: self.census,
        }
    }

    /// Merges this solve's census and piece log into the engine collector
    /// (one lock, at exit).
    pub fn merge_into(&self, tele: &WalkTelemetry) {
        tele.merge_tally(self);
    }
}

// ---------------------------------------------------------------------------
// WalkTelemetry — the engine-owned collector
// ---------------------------------------------------------------------------

/// The engine-owned walk-telemetry collector.
///
/// `Arc`-constructed by the engine beside `WalkMemo` and passed as
/// `Option<&WalkTelemetry>` into the solve entries; `None` means an offline run
/// that wants no cumulative census. It holds the process-cumulative
/// [`WalkNsCensus`] plus the census-piece log under one `parking_lot::Mutex`. A
/// solve merges its [`WalkTally`] into it exactly once — never per bump.
///
/// Not feature-gated: the type and its signatures exist in every build so the
/// solve entries keep one shape. No environment reads.
#[derive(Default)]
pub struct WalkTelemetry {
    inner: parking_lot::Mutex<WalkTelemetryState>,
}

/// The collector's guarded interior.
#[derive(Debug, Default)]
struct WalkTelemetryState {
    ns: WalkNsCensus,
    census_pieces: Vec<(Vec<usize>, U256)>,
}

impl WalkTelemetry {
    /// A fresh, all-zero collector (no environment reads).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges one solve's tally: adds its census and appends its census pieces,
    /// under a single lock.
    pub fn merge_tally(&self, tally: &WalkTally) {
        let mut guard = self.inner.lock();
        guard.ns.accumulate(tally.ns);
        guard.census_pieces.extend_from_slice(&tally.census_pieces);
    }

    /// Swaps the cumulative census back to zero and returns what it held.
    #[must_use]
    pub fn drain_ns(&self) -> WalkNsCensus {
        std::mem::take(&mut self.inner.lock().ns)
    }

    /// Drains the census-piece log, leaving it empty.
    #[must_use]
    pub fn take_census_pieces(&self) -> Vec<(Vec<usize>, U256)> {
        std::mem::take(&mut self.inner.lock().census_pieces)
    }
}
