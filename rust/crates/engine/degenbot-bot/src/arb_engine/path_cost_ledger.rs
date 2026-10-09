//! The path cost ledger: the solve cycle's single owner of per-path cost
//! accounting (commit 2 of PR C4).
//!
//! [`PathCostLedger`] absorbs the live sinks that `lane_walk::solve_one_path`'s
//! fan-out used to touch one by one:
//!
//! - the pipeline instruments (`observe_per_path_solve_duration` /
//!   `observe_per_path_gate_duration`),
//! - the K-slowest retention (a `BinaryHeap` of [`PathTimeRecord`] — the
//!   former ten-slot positional tuple, now a named struct),
//! - the two heavy-path capture hooks (`capture` / `capture_mixed`).
//!
//! The lane walk makes ONE [`PathCostLedger::record`] call; the fan-out lives
//! here. The tracing span attributes (`path.us` / `path.sims` / `path.pieces`
//! / `gate.us` / `path.profit`) stay on the span — those are tracing, not
//! ledger state, and are recorded by the walk.
//!
//! NOT owned here: the next-cycle bin-packing cost history
//! (`sims_recorder` / `gate_recorder`, the LPT history) and the Q3-DENSE
//! alert — both live on later commits of PR C4.

use super::solver_capture::HeavyPathCapture;
use std::sync::Arc;

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
}

impl PathCostLedger {
    /// Build the per-cycle ledger: the solve block plus the two capture
    /// hooks (engine-owned, snapshotted once per cycle).
    pub(crate) fn new(
        solve_block: u64,
        capture: Option<Arc<HeavyPathCapture>>,
        capture_mixed: Option<Arc<HeavyPathCapture>>,
    ) -> Self {
        Self {
            solve_block,
            capture,
            capture_mixed,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
        }
    }

    /// The lane walk's ONE ledger call: record one solved path's cost facts —
    /// the pipeline instruments, the K-slowest retention, and the capture
    /// triggers. The walk computes `elapsed` ONCE around the solve and also
    /// uses it for the tracing span attributes.
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
        let (pieces, sims) = (ws.pieces, ws.sims);

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
