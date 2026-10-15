//! The path cost ledger: the solve cycle's single owner of per-path cost
//! accounting (commits 2-3 of PR C4).
//!
//! [`PathCostLedger`] absorbs the live sinks that `lane_walk::solve_one_path`'s
//! fan-out used to touch one by one:
//!
//! - the pipeline instruments (`observe_per_path_solve_duration` /
//!   `observe_per_path_gate_duration`),
//! - the K-slowest retention (a `BinaryHeap` of [`PathTimeRecord`] — the
//!   former ten-slot positional tuple, now a named struct),
//! - the two heavy-path capture hooks (`capture` / `capture_mixed`),
//! - the next-cycle LPT cost history ([`PathCostHistory`]).
//!
//! The lane walk makes ONE [`PathCostLedger::record`] call; the fan-out lives
//! here. The tracing span attributes (`path.us` / `path.sims` / `path.pieces`
//! / `gate.us` / `path.profit`) stay on the span — those are tracing, not
//! ledger state, and are recorded by the walk.
//!
//! It also owns the next-cycle bin-packing cost history (commit 3): the LPT
//! carry-over the walk writes per path via [`PathCostLedger::record`] and the
//! bin packer reads once per cycle as a typed [`CostSnapshot`]. The old raw
//! per-path maps and the `lock().clone()` snapshot idiom live behind
//! [`PathCostHistory::record`] / [`PathCostHistory::snapshot`].
//!
//! It also owns the Q3-DENSE one-shot alert (commit 4 of PR C4): the walk
//! reports each path's dense-word counter on the outcome, and the alert
//! policy plus its once-per-process latch live HERE beside the other sinks.

use super::solver_capture::HeavyPathCapture;
use super::HashMap;
use degenbot_core::op_warn;
use std::sync::Arc;

/// The Q3 dense one-shot latch: one alert per process however many cycles
/// cross the threshold. The decision lives in [`PathCostLedger::record`] and
/// this latch rides with it.
static DENSE_ALERTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How many slowest-path entries the retention keeps (a min-heap keeps this
/// O(K) memory).
const SLOWEST_PATHS_K: usize = 5;

/// One K-slowest-path attribution row — lets a completion event name the cost
/// driver of the slowest routes: gate-envelope bound composition (with its
/// derive/compose/search phase split) vs the walk proper, not just wall time.
///
/// Field order is load-bearing: [`PathTimeRecord`] derives `Ord` on its
/// declaration order, byte-matching the former positional tuple's
/// lexicographic ordering so the `Reverse` heap ranks rows identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct PathTimeRecord {
    /// Measured per-path solve wall time (microseconds).
    pub(crate) micros: u128,
    /// CL tick-range pieces visited by the monotone walk.
    pub(crate) pieces: u64,
    /// Full path simulations.
    pub(crate) sims: u64,
    /// Word-boundary steps inside every sim.
    pub(crate) word_steps: u64,
    /// Stop-time refinement probes.
    pub(crate) refine_sims: u64,
    /// Gate-envelope wall time (microseconds).
    pub(crate) gate_us: u64,
    /// Gate derivation phase (microseconds).
    pub(crate) gate_derive_us: u64,
    /// Gate compose phase (microseconds).
    pub(crate) gate_compose_us: u64,
    /// Gate search phase (microseconds).
    pub(crate) gate_search_us: u64,
    /// The path id this row attributes.
    pub(crate) path_id: u64,
}

/// Min-heap (via `Reverse`) keeping only the K slowest paths in O(K) memory.
type PathTimesHeap = std::collections::BinaryHeap<std::cmp::Reverse<PathTimeRecord>>;

/// One path's MEASURED cost from the previous cycle: the walk's full-sim
/// count and the gate envelope's micros. The LPT predictor reads both terms as
/// one row (same micros scale, see `workload_partition::sims_aware_cost`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PathCost {
    /// Full path simulations measured on the path's last solve.
    pub(crate) sims: u64,
    /// Gate-envelope micros measured on the path's last solve.
    pub(crate) gate_us: u64,
}

impl PathCost {
    /// One measured row: the walk sims plus the gate micros.
    pub(crate) fn new(sims: u64, gate_us: u64) -> Self {
        Self { sims, gate_us }
    }
}

/// The ledger's durable LPT carry-over: the per-path cost each cycle's walk
/// writes, read back by the NEXT cycle's bin packer. It outlives the cycle
/// that writes it, so the engine owns one and the per-cycle [`PathCostLedger`]
/// holds the handle.
#[derive(Default)]
pub(crate) struct PathCostHistory {
    rows: parking_lot::Mutex<HashMap<u64, PathCost>>,
}

impl PathCostHistory {
    /// An empty history — no path has a measured previous-cycle cost yet
    /// (a cold start bins on the structural proxy alone).
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Write one path's measured cost. [`PathCostLedger::record`] calls this
    /// per solved path; it is also the seed seam for a test that pins an LPT
    /// placement (a huge sims cost sorts the path first into its own bin).
    pub(crate) fn record(&self, pid: u64, cost: PathCost) {
        self.rows.lock().insert(pid, cost);
    }

    /// The ONE lock per cycle: freeze the history into the typed snapshot the
    /// bin packer queries.
    pub(crate) fn snapshot(&self) -> CostSnapshot {
        CostSnapshot {
            costs: self.rows.lock().clone(),
        }
    }
}

/// One cycle's frozen view of the LPT carry-over, immutable by convention:
/// the bin packer only queries it, never mutates it.
pub(crate) struct CostSnapshot {
    costs: HashMap<u64, PathCost>,
}

impl CostSnapshot {
    /// The previous-cycle measured cost for `pid`, if the path solved then.
    pub(crate) fn cost(&self, pid: u64) -> Option<PathCost> {
        self.costs.get(&pid).copied()
    }
}

/// The per-cycle per-path cost sink, owned as ONE field on
/// `solve_cycle::SolveCycleShared`.
pub(crate) struct PathCostLedger {
    /// The solve block the cycle anchors to (the capture hooks' block stamp).
    solve_block: u64,
    /// Optional offline all-CL heavy-path capture hook.
    capture: Option<Arc<HeavyPathCapture>>,
    /// Optional offline mixed V2+CL heavy-path capture hook.
    capture_mixed: Option<Arc<HeavyPathCapture>>,
    /// The K-slowest retention.
    path_times: parking_lot::Mutex<PathTimesHeap>,
    /// The engine-owned LPT carry-over this cycle's per-path records feed.
    history: Arc<PathCostHistory>,
}

impl PathCostLedger {
    /// Build the per-cycle ledger: the solve block, the two capture hooks
    /// (engine-owned, snapshotted once per cycle), and the handle to the
    /// engine-owned LPT carry-over.
    pub(crate) fn new(
        solve_block: u64,
        capture: Option<Arc<HeavyPathCapture>>,
        capture_mixed: Option<Arc<HeavyPathCapture>>,
        history: Arc<PathCostHistory>,
    ) -> Self {
        Self {
            solve_block,
            capture,
            capture_mixed,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
            history,
        }
    }

    /// The ONE snapshot per cycle: the typed LPT carry-over the bin packer
    /// reads (`cost(pid)` per path), taken before the cycle's own walk runs.
    pub(crate) fn snapshot(&self) -> CostSnapshot {
        self.history.snapshot()
    }

    /// The lane walk's ONE ledger call: record one solved path's cost facts —
    /// the pipeline instruments, the K-slowest retention, the capture
    /// triggers, and this block's LPT cost row for the next cycle. The walk
    /// computes `elapsed` ONCE around the solve and also uses it for the
    /// tracing span attributes.
    pub(crate) fn record(
        &self,
        pid: u64,
        resolved: &::degenbot_solvers::mixed::ResolvedMixedPath,
        outcome: &::degenbot_solvers::mixed::solve::SolveOutcome,
        elapsed: std::time::Duration,
    ) {
        let micros = elapsed.as_micros();
        let gs = outcome.gate_stats;
        let gate_us = u64::try_from(gs.duration_ns / 1_000).unwrap_or(u64::MAX);

        if let Some(p) = crate::instruments::pipeline() {
            #[expect(clippy::cast_precision_loss)]
            {
                p.observe_per_path_solve_duration(micros as f64 / 1e6);
                p.observe_per_path_gate_duration(gs.duration_ns as f64 / 1e9);
            }
        }

        let ws = outcome.stats;

        // The Q3 dense one-shot alert: the first path whose CL walk crossed the
        // dense-word threshold logs once per process (harvest a real capture).
        // Same latch + threshold as the retired walk-side check; the threshold
        // constant stays in the solver crate.
        if ws.max_dense_words >= ::degenbot_solvers::concentrated_liquidity::DENSE_OBSERVE_THRESHOLD
            && !DENSE_ALERTED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            op_warn!(
                domain = solver,
                max_dense_words = ws.max_dense_words,
                threshold = ::degenbot_solvers::concentrated_liquidity::DENSE_OBSERVE_THRESHOLD,
                "Q3-DENSE: a CL range crossed the dense-word threshold; harvest a real capture"
            );
        }

        let (pieces, sims) = (ws.pieces, ws.sims);

        // The next cycle's LPT cost: this block's measured sims and gate
        // micros (the walk no longer writes them itself).
        self.history.record(
            pid,
            PathCost::new(u64::try_from(sims).unwrap_or(0), gate_us),
        );

        self.retain_slowest(PathTimeRecord {
            micros,
            pieces: u64::try_from(pieces).unwrap_or(0),
            sims: u64::try_from(sims).unwrap_or(0),
            word_steps: u64::try_from(ws.word_steps).unwrap_or(0),
            refine_sims: u64::try_from(ws.refine_sims).unwrap_or(0),
            gate_us,
            gate_derive_us: u64::try_from(gs.derive_ns / 1_000).unwrap_or(u64::MAX),
            gate_compose_us: u64::try_from(gs.compose_ns / 1_000).unwrap_or(u64::MAX),
            gate_search_us: u64::try_from(gs.search_ns / 1_000).unwrap_or(u64::MAX),
            path_id: pid,
        });

        // The two capture hooks keep their once-per-cycle snapshot order
        // (all-CL first, mixed second).
        for cap in [self.capture.as_ref(), self.capture_mixed.as_ref()]
            .into_iter()
            .flatten()
        {
            cap.maybe_capture(
                pid,
                self.solve_block,
                u64::try_from(micros).unwrap_or(u64::MAX),
                u64::try_from(sims).unwrap_or(0),
                u64::try_from(pieces).unwrap_or(0),
                outcome.result.as_ref(),
                resolved,
            );
        }
    }

    /// Keep only the K slowest rows seen so far (min-heap by `micros` via
    /// `Reverse`).
    fn retain_slowest(&self, record: PathTimeRecord) {
        let mut heap = self.path_times.lock();
        let worst = heap
            .peek()
            .map_or(u128::MAX, |std::cmp::Reverse(r)| r.micros);
        if heap.len() < SLOWEST_PATHS_K || record.micros > worst {
            heap.push(std::cmp::Reverse(record));
            if heap.len() > SLOWEST_PATHS_K {
                heap.pop();
            }
        }
    }
}
