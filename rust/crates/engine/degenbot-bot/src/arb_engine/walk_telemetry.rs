//! The per-epoch `WalkMemoStats` tap (observation only).
//!
//! [`WalkMemo`](::degenbot_solvers::cl::WalkMemo) has been fully instrumented
//! since the memo landed (probes, hits, distinct, cache plays, negatives,
//! per-probe/per-hit sims), but nothing in the engine drained it: `take_stats`
//! existed with no engine-side caller. This module is the drain. It owns:
//!
//! * the **boundary drain** — one `take_stats()` per `begin_block` call site
//!   (the engine has exactly one: `SolveCycle::run_epoch`), issued BEFORE the
//!   memo advances. `take_stats` resets the counters it returns, so
//!   drain-then-advance is the only order that captures the epoch that just
//!   ended, and one drain per boundary is the whole contract — never more.
//! * the **final drain** — the same tap at engine teardown
//!   (`EngineDriver::stop` → [`super::engine_stages::EngineStages`], which
//!   locks the engine and calls [`final_drain`]), so the LAST epoch's
//!   counters are not lost to the boundary that never comes after it.
//!
//! Both entry points are one-liners at their call sites; everything else
//! (gating, rendering, the two emission channels, the failure posture) lives
//! here so the unit tests have one home.
//!
//! # Emission channels
//!
//! Every emitted epoch goes to two channels:
//!
//! 1. one JSON line appended to `logs/walkmemo_stats.jsonl` (path resolved
//!    against the process CWD — `run_bot.sh` cds to the workspace root and
//!    already writes `logs/bot_run.log` into the same `logs/` directory), and
//! 2. the ADR-043 instruments `degenbot.solver.walk_memo_probes` / `_hits` /
//!    `_cache_plays` / `_negatives_played` (counters) and
//!    `degenbot.solver.walk_memo_negative_entries` (gauge), built in
//!    `crate::instruments` behind the `otel` feature; default builds no-op
//!    through the stub pipeline, exactly like every other observation site.
//!
//! # The gate: when the tap emits nothing (and why)
//!
//! A drained record is emitted only when it carries ACTIVITY: any accounting
//! counter nonzero (`probes`, `hits`, `cache_plays`, `negatives_played`,
//! `probes_sims`, `hits_sims`). The live gauges (`distinct`,
//! `negative_entries`) ride along on active lines but never trigger emission
//! on their own: they are running values already reported by the last active
//! line, and a gauge-only line from a repeated drain of the same epoch would
//! be a duplicate row. One rule covers the three shapes the brief names:
//!
//! * **The first boundary of a run** — the memo boots with epoch 0 and every
//!   counter at zero, so the first `begin_block` has no prior epoch to
//!   report. Documented choice: SKIP (emit nothing), not an empty line — an
//!   all-zero row carries no information and would open every JSONL with a
//!   misleading epoch 0.
//! * **A disabled memo** — `WalkMemo::new(false, false)` (every test
//!   scaffold) never counts: `probe`/`store`/`note_cost` no-op under both
//!   flags off and the solve entries probe only under the memo's `active()`
//!   stance, so every counter stays zero and the tap is fully inert — no
//!   JSONL line, no instruments, no IO. The bot-side gate is this
//!   zero-activity short-circuit, NOT the memo's `active()` itself:
//!   `active()` is `pub(super)` in `degenbot-solvers` (crate-private) and
//!   unreachable from `degenbot-bot`; the zero counters ARE that stance's
//!   observable effect. (`begin_block` still advances a disabled memo's
//!   epoch — irrelevant here: nothing else ever moves.)
//! * **A quiet epoch** — a block where nothing probed emits no line.
//!
//! # Line semantics (read before aggregating)
//!
//! Lines are appended across runs; each line carries `epoch` + `block` +
//! `ts`, which is what makes successive capture runs separable (see the
//! writer doc on [`JsonlSink`]). Field meanings:
//!
//! * `epoch` — the memo's epoch the drained counters belong to. The memo's
//!   epoch contract names the epoch with the engine-passed SOLVED block
//!   number, so `epoch` is the solved block whose probes/stores these are.
//! * `block` — on a boundary line, the INCOMING solve block whose
//!   `begin_block` triggered the drain (the first block of the NEXT epoch):
//!   normally `epoch + 1` (steady per-block cadence); `block == epoch` marks
//!   a repeat boundary (an equal-epoch `begin_block` no-op); `block < epoch`
//!   marks a reorg rewind that held the higher epoch. On a final-drain line
//!   there is no incoming block, so `block` equals `epoch`.
//! * `reason` — `"epoch_boundary"` or `"final_drain"`; the final epoch's
//!   line is a partial by construction (its epoch never sees a boundary).
//!
//! Those no-op boundaries still drain exactly once per the contract above,
//! so a still-live epoch can appear on TWO lines — a partial followed by the
//! remainder. When aggregating in the readout: sum the counters per epoch,
//! take the gauges (`distinct`, `negative_entries`) last-value-wins.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ::degenbot_solvers::cl::{WalkMemo, WalkMemoStats};
use degenbot_core::op_warn;

/// The production JSONL path, relative to the process CWD. `run_bot.sh` cds
/// to the workspace root and writes `logs/bot_run.log` under the same
/// directory, so the capture run's artifacts land together.
const STATS_JSONL_PATH: &str = "logs/walkmemo_stats.jsonl";

/// Why a record was drained — carried on the line so the readout can tell a
/// boundary-flushed epoch from the teardown-flushed partial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrainReason {
    /// A `begin_block` boundary drained the epoch that just ended.
    EpochBoundary,
    /// Engine teardown drained the final (never-boundaried) epoch.
    FinalDrain,
}

impl DrainReason {
    /// The stable JSON spelling.
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::EpochBoundary => "epoch_boundary",
            Self::FinalDrain => "final_drain",
        }
    }
}

/// One emission target. Production appends to the JSONL file; the unit tests
/// capture lines so `cargo test` never touches the real `logs/` tree.
pub(crate) trait StatsSink: Send + Sync {
    /// Append one rendered JSON line (the sink owns the trailing newline).
    fn emit(&self, line: &str);
}

/// Production sink: appends one line per emitted epoch to the JSONL path.
///
/// # Append semantics (the writer contract)
///
/// The file is opened in append mode (and the `logs/` directory created if
/// absent — `run_bot.sh` pre-creates it, a bare engine run may not), so runs
/// append rather than truncate. Separability across runs comes from the line
/// content itself: every line carries `epoch` + `block` + `ts`, so a capture
/// run is the line range between its first and last timestamp, and two runs
/// never alias one another's epochs unless they solved the same blocks.
/// Nothing here rotates or prunes; deleting the file between runs is an
/// operator action for a clean file.
///
/// Writes are best-effort: a failed append warns on the solver domain and
/// continues — an observation channel must never fail the solve cycle.
struct JsonlSink {
    path: PathBuf,
}

impl JsonlSink {
    /// The production sink at [`STATS_JSONL_PATH`] under the process CWD.
    fn production() -> Self {
        Self {
            path: PathBuf::from(STATS_JSONL_PATH),
        }
    }
}

impl StatsSink for JsonlSink {
    fn emit(&self, line: &str) {
        if let Err(err) = append_jsonl_line(&self.path, line) {
            op_warn!(
                domain = solver,
                error = %err,
                path = %self.path.display(),
                "walk memo stats JSONL append failed (observation is best-effort; the solve cycle continues)"
            );
        }
    }
}

/// Append one line, creating the parent directory (`logs/`) when absent.
fn append_jsonl_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// THE boundary tap (the `run_epoch` one-liner): drain the epoch that just
/// ended BEFORE `begin_block(incoming_block)` advances it. Call exactly once
/// per `begin_block` site — `take_stats` resets, so a second call would
/// split the epoch's counters across two lines.
///
/// Emission is gated (see the module docs): a first boundary, a disabled
/// memo, and quiet epochs emit nothing.
pub(crate) fn drain_epoch_boundary(memo: &WalkMemo, incoming_block: u64) {
    drain_epoch_boundary_into(memo, incoming_block, &JsonlSink::production());
}

/// Test/injection twin of [`drain_epoch_boundary`]: the same drain against a
/// caller-owned sink (the unit tests capture lines instead of writing the
/// production `logs/` path).
pub(crate) fn drain_epoch_boundary_into(
    memo: &WalkMemo,
    incoming_block: u64,
    sink: &dyn StatsSink,
) {
    let stats = memo.take_stats();
    emit_if_active(
        &stats,
        DrainReason::EpochBoundary,
        Some(incoming_block),
        sink,
    );
}

/// THE teardown tap (the `EngineDriver::stop` one-liner via
/// `EngineStages`): drain the LAST epoch, whose boundary never comes because
/// the pump is already down. Same gate as the boundary drain; a
/// stop-before-resume run (no solving) emits nothing.
pub(crate) fn final_drain(memo: &WalkMemo) {
    final_drain_into(memo, &JsonlSink::production());
}

/// Test/injection twin of [`final_drain`].
pub(crate) fn final_drain_into(memo: &WalkMemo, sink: &dyn StatsSink) {
    let stats = memo.take_stats();
    emit_if_active(&stats, DrainReason::FinalDrain, None, sink);
}

/// The activity gate: does this drained record carry anything worth a line?
/// Counters only — the gauges ride on active lines (module docs name the
/// three shapes this answers, and why a gauge alone never emits).
#[must_use]
fn is_active(stats: &WalkMemoStats) -> bool {
    stats.probes != 0
        || stats.hits != 0
        || stats.cache_plays != 0
        || stats.negatives_played != 0
        || stats.probes_sims != 0
        || stats.hits_sims != 0
}

/// Gate, render, emit — the one path both tap entry points share.
fn emit_if_active(
    stats: &WalkMemoStats,
    reason: DrainReason,
    incoming_block: Option<u64>,
    sink: &dyn StatsSink,
) {
    if !is_active(stats) {
        return;
    }
    let line = render_line(stats, reason, incoming_block);
    sink.emit(&line);
    record_instruments(stats);
}

/// Render one JSONL line. The JSON object (keys + types) is the readout
/// contract; key order is presentation only.
#[must_use]
fn render_line(stats: &WalkMemoStats, reason: DrainReason, incoming_block: Option<u64>) -> String {
    // Final-drain lines have no incoming block: the drained epoch IS the
    // last solved block the memo knew.
    let block = incoming_block.unwrap_or(stats.epoch);
    serde_json::json!({
        "ts": rfc3339_now(),
        "reason": reason.label(),
        "epoch": stats.epoch,
        "block": block,
        "probes": stats.probes,
        "hits": stats.hits,
        "distinct": stats.distinct,
        "cache_plays": stats.cache_plays,
        "negative_entries": stats.negative_entries,
        "negatives_played": stats.negatives_played,
        "probes_sims": stats.probes_sims,
        "hits_sims": stats.hits_sims,
    })
    .to_string()
}

/// Wall-clock UTC timestamp (RFC 3339, second resolution — epochs are
/// block-cadence, seconds apart; sub-second precision would be noise). Falls
/// back to the Unix epoch when the clock is behind it: a non-monotonic clock
/// must not fail the drain.
fn rfc3339_now() -> String {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    degenbot_rpc::cassette::rfc3339_utc(unix_secs)
}

/// The ADR-043 channel: one epoch's counters add as deltas (they are
/// per-epoch and were just reset inside the memo), the running
/// `negative_entries` gauge records last-value-wins.
fn record_instruments(stats: &WalkMemoStats) {
    if let Some(pipeline) = crate::instruments::pipeline() {
        pipeline.add_walk_memo_epoch(
            stats.probes,
            stats.hits,
            stats.cache_plays,
            stats.negatives_played,
            stats.negative_entries,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::alloy::primitives::U256;
    use ::degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
    use ::degenbot_solvers::cl::{solve_cl_piecewise, ClSolveTables};
    use ::degenbot_solvers::runtime::SolveRuntimeConfig;
    use parking_lot::Mutex;

    /// Capturing sink — lines land in a buffer, never the production
    /// `logs/` path (the whole point of the `_into` seams).
    #[derive(Default)]
    struct VecSink(Mutex<Vec<String>>);

    impl VecSink {
        fn lines(&self) -> parking_lot::MutexGuard<'_, Vec<String>> {
            self.0.lock()
        }
    }

    impl StatsSink for VecSink {
        fn emit(&self, line: &str) {
            self.lines().push(line.to_owned());
        }
    }

    /// Parse a captured line (render contract: every line is one JSON
    /// object carrying the full field set).
    #[expect(
        clippy::expect_used,
        reason = "test assertion on a render bug must fail loudly"
    )]
    fn parse_line(line: &str) -> serde_json::Value {
        serde_json::from_str(line).expect("captured line is valid JSON")
    }

    /// One well-formed single-range CL sequence: every solve of it folds the
    /// same fingerprint, so consecutive epochs drive the memo's
    /// miss → commit → hit path deterministically.
    #[expect(
        clippy::expect_used,
        reason = "test fixture; a construction bug must fail loudly"
    )]
    fn one_hop_sequence() -> IntV3TickRangeSequence {
        IntV3TickRangeSequence::new(vec![IntV3TickRangeHop {
            liquidity: 10_u128.pow(18),
            sqrt_price_x96: U256::from(1u128 << 96),
            sqrt_price_lower_x96: U256::from(1u128 << 95),
            sqrt_price_upper_x96: U256::from(1u128 << 97),
            gamma_numer: 997_000,
            fee_denom: 1_000_000,
            zero_for_one: true,
            word_boundary_prices: Vec::new(),
        }])
        .expect("one consistent hop builds a sequence")
    }

    /// Solve the fixture once against the memo: the entry probes under
    /// `active()` (the only outside-reachable probe/store drive — probe,
    /// `note_cost`, store are `pub(super)` in the solvers crate), so one call
    /// is exactly one probe + one cache consult + a commit.
    fn solve_once(memo: &WalkMemo, seq: &IntV3TickRangeSequence, tables: &ClSolveTables) {
        let cfg = SolveRuntimeConfig::default();
        let _ = solve_cl_piecewise(&[seq], std::slice::from_ref(tables), Some(memo), &cfg, None);
    }

    #[test]
    fn first_boundary_and_disabled_memo_emit_nothing() {
        let sink = VecSink::default();

        // Boot state of an ENABLED memo: epoch 0, all counters zero — the
        // first begin_block has no prior epoch and must emit nothing.
        let memo = WalkMemo::new(true, true);
        drain_epoch_boundary_into(&memo, 100, &sink);
        assert!(sink.lines().is_empty(), "first boundary must stay silent");

        // A DISABLED memo (the test scaffolds' constructor) across three
        // boundaries plus a final drain: begin_block still advances its
        // epoch, but no counter ever moves — the tap stays fully inert.
        let off = WalkMemo::new(false, false);
        off.begin_block(100);
        drain_epoch_boundary_into(&off, 101, &sink);
        off.begin_block(101);
        drain_epoch_boundary_into(&off, 102, &sink);
        off.begin_block(102);
        final_drain_into(&off, &sink);
        assert!(
            sink.lines().is_empty(),
            "disabled memo must never emit: {:?}",
            *sink.lines()
        );
    }

    #[test]
    fn epochs_drain_per_boundary_with_counters_reset_between_lines() {
        let sink = VecSink::default();
        let memo = WalkMemo::new(true, true);
        let seq = one_hop_sequence();
        let tables = ClSolveTables::derive(&seq);

        // --- epoch 100: two solves of the same composition ---------------
        drain_epoch_boundary_into(&memo, 100, &sink); // boot: silent
        memo.begin_block(100);
        solve_once(&memo, &seq, &tables);
        solve_once(&memo, &seq, &tables);

        // Boundary into 101 drains epoch 100: two probes, one distinct
        // fingerprint, two cache consults, no hits (the previous census was
        // empty).
        drain_epoch_boundary_into(&memo, 101, &sink);
        memo.begin_block(101);
        let first = parse_line(&sink.lines()[0]);
        assert_eq!(first["reason"], "epoch_boundary");
        assert_eq!(first["epoch"], 100);
        assert_eq!(first["block"], 101);
        assert_eq!(first["probes"], 2);
        assert_eq!(first["hits"], 0);
        assert_eq!(first["distinct"], 1);
        assert_eq!(first["cache_plays"], 2);
        assert!(first["ts"].as_str().is_some_and(|ts| !ts.is_empty()));

        // --- epoch 101: the same composition now replays from the census --
        solve_once(&memo, &seq, &tables);
        solve_once(&memo, &seq, &tables);

        // Boundary into 102 drains epoch 101: the counters RESET between
        // lines (probes 2, not 4) and the cross-block census now reports
        // hits — the exact per-epoch separation this tap exists for.
        drain_epoch_boundary_into(&memo, 102, &sink);
        memo.begin_block(102);
        let second = parse_line(&sink.lines()[1]);
        assert_eq!(second["epoch"], 101);
        assert_eq!(second["block"], 102);
        assert_eq!(second["probes"], 2);
        assert_eq!(second["hits"], 2);
        assert_eq!(second["distinct"], 1);
        assert_eq!(second["cache_plays"], 2);
    }

    #[test]
    fn final_drain_emits_the_last_epoch() {
        let sink = VecSink::default();
        let memo = WalkMemo::new(true, true);
        let seq = one_hop_sequence();
        let tables = ClSolveTables::derive(&seq);

        memo.begin_block(500);
        solve_once(&memo, &seq, &tables);

        // Engine stop: no boundary follows epoch 500, so the FINAL drain is
        // the only thing standing between these counters and oblivion. The
        // line is the partial-by-construction shape (reason final_drain,
        // block == epoch).
        final_drain_into(&memo, &sink);
        let last = parse_line(&sink.lines()[0]);
        assert_eq!(last["reason"], "final_drain");
        assert_eq!(last["epoch"], 500);
        assert_eq!(last["block"], 500);
        assert_eq!(last["probes"], 1);
        assert_eq!(last["hits"], 0);
        assert_eq!(last["cache_plays"], 1);

        // The drain consumed the counters: a second final drain finds an
        // empty epoch and stays silent (guards a latched double-stop).
        final_drain_into(&memo, &sink);
        assert_eq!(sink.lines().len(), 1, "post-drain epoch is quiet");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a render-contract bug (line is not a JSON object) must fail loudly"
    )]
    fn every_line_carries_the_full_field_set() {
        let sink = VecSink::default();
        let memo = WalkMemo::new(true, true);
        let seq = one_hop_sequence();
        let tables = ClSolveTables::derive(&seq);

        memo.begin_block(7);
        solve_once(&memo, &seq, &tables);
        drain_epoch_boundary_into(&memo, 8, &sink);

        let value = parse_line(&sink.lines()[0]);
        let object = value.as_object().expect("line is a JSON object");
        for key in [
            "ts",
            "reason",
            "epoch",
            "block",
            "probes",
            "hits",
            "distinct",
            "cache_plays",
            "negative_entries",
            "negatives_played",
            "probes_sims",
            "hits_sims",
        ] {
            assert!(object.contains_key(key), "line missing {key}: {value}");
        }
    }
}
