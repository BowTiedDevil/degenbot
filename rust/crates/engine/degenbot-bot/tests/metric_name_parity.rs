//! The frozen metric-name parity gate (ADR-083).
//!
//! `instruments.rs` is the one owner of the metric names, and those names are
//! COUPLED verbatim to Grafana dashboards and alert queries (e.g.
//! `degenbot_epoch_header_to_publish_seconds`). This gate pins the closed
//! name set bidirectionally against the construction calls in
//! `instruments.rs`: a rename in code without moving the dashboard + alert
//! consumers fails HERE — the same moment the developer is editing the
//! census, not a silent scrape regression.

use std::path::Path;

/// The frozen `OTel` instrument names `instruments.rs` may construct. Add a
/// name ONLY in the same change that moves every dashboard/alert consumer
/// (see the PROMETHEUS NAME COUPLING notes on the fields).
const FROZEN_NAMES: &[&str] = &[
    "degenbot.backfills.executed",
    "degenbot.backrun.feed.connected",
    "degenbot.backrun.feed.dropped_ring",
    "degenbot.backrun.feed.frames",
    "degenbot.backrun.feed.reconnects",
    "degenbot.backrun.feed.rejected_chain_id",
    "degenbot.backrun.feed.rejected_parse",
    "degenbot.backrun.feed.seconds_since_event",
    "degenbot.backrun.frame.observed",
    "degenbot.block.header_to_first_log",
    "degenbot.block.log_burst",
    "degenbot.block.settle_wait",
    "degenbot.blocks.observed",
    "degenbot.candidates.found",
    "degenbot.cgroup.throttled",
    "degenbot.detached.applied",
    "degenbot.detached.degraded_cycles",
    "degenbot.detached.in_flight",
    "degenbot.detached.leads_expired",
    "degenbot.detached.merge_panic",
    "degenbot.detached.send_failed",
    "degenbot.detached.shed",
    "degenbot.detached.stale_dropped",
    "degenbot.dispatch.gas_used",
    "degenbot.dispatch.gross_profit",
    "degenbot.dispatch.net_profit",
    "degenbot.engine.quarantined_pools",
    "degenbot.engine.registered_paths",
    "degenbot.epoch.header_to_publish",
    "degenbot.epoch.logs_applied",
    "degenbot.epoch.logs_block",
    "degenbot.epoch.logs_ignored",
    "degenbot.epoch.logs_received",
    "degenbot.epoch.logs_seen",
    "degenbot.epoch.stale_drops",
    "degenbot.errors",
    "degenbot.fleet.intake_backlog",
    "degenbot.fleet.profile",
    "degenbot.late_log.admitted",
    "degenbot.log.decode",
    "degenbot.logs.applied",
    "degenbot.logs.apply_missed",
    "degenbot.logs.decoded",
    "degenbot.logs.received",
    "degenbot.logs.undecoded",
    "degenbot.monitor.outcomes",
    "degenbot.process.rss",
    "degenbot.pump.seconds_since_apply",
    "degenbot.pump.seconds_since_header",
    "degenbot.quiesce.window_ms",
    "degenbot.registration.skips",
    "degenbot.reorg.depth_blocks",
    "degenbot.reorg.recovery_dropped",
    "degenbot.reorg.unwound_pools",
    "degenbot.reorg.windows",
    "degenbot.sim.error_reason",
    "degenbot.simulate.duration",
    "degenbot.simulate.verdicts",
    "degenbot.solve.duration",
    "degenbot.solve.gate_duration",
    "degenbot.solve.mutex_hold",
    "degenbot.solve.path_duration",
    "degenbot.solver.clamps",
    "degenbot.solver.walk_memo_cache_plays",
    "degenbot.solver.walk_memo_hits",
    "degenbot.solver.walk_memo_negative_entries",
    "degenbot.solver.walk_memo_negatives_played",
    "degenbot.solver.walk_memo_probes",
    "degenbot.solves.executed",
    "degenbot.stage.publish_cycle",
    "degenbot.stage.rewind",
    "degenbot.stage.rewind_duration",
    "degenbot.stage.streaming_age",
    "degenbot.state.apply",
    "degenbot.state.head_lag_blocks",
    "degenbot.state_lock.hold",
    "degenbot.state_lock.wait",
    "degenbot.submit.latency",
    "degenbot.submit.outcomes",
    "degenbot.updater.rpc_round_trips",
    "degenbot.updater.stage.duration",
    "degenbot.updater.write_lock_hold",
    "degenbot.updater.write_lock_hold_last",
    "degenbot.worker.census",
    "degenbot.ws.logs.seen",
];

fn instruments_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/instruments.rs");
    #[expect(clippy::expect_used)] // the census module's source is part of the crate
    std::fs::read_to_string(path).expect("instruments.rs is readable")
}

/// Every instrument constructed in `instruments.rs` is in the frozen set.
/// live-invariant gate (ADR-087 D2): watches every instrument name the live `instruments.rs` constructs against the frozen census; not tripwire precedent.
#[test]
fn every_constructed_name_is_frozen() {
    let source = instruments_source();
    let mut constructed: Vec<&str> = Vec::new();
    for ctor in [
        "f64_histogram(\"",
        "u64_counter(\"",
        "f64_gauge(\"",
        "u64_gauge(\"",
        "u64_histogram(\"",
    ] {
        let mut rest = source.as_str();
        while let Some(idx) = rest.find(ctor) {
            let after = &rest[idx + ctor.len()..];
            if let Some(end) = after.find('"') {
                constructed.push(&after[..end]);
                rest = &after[end..];
            } else {
                break;
            }
        }
    }
    let mut unnamed: Vec<&str> = constructed
        .iter()
        .copied()
        .filter(|name| !FROZEN_NAMES.contains(name))
        .collect();
    unnamed.sort_unstable();
    unnamed.dedup();
    assert!(
        unnamed.is_empty(),
        "instrument names constructed but not frozen (move the dashboard + \n  alert consumers in the SAME change): {unnamed:?}"
    );
}

/// Every frozen name is actually constructed — a stale frozen row is a lie
/// about the census (and a dashboard querying a retired metric).
/// live-invariant gate (ADR-087 D2): watches every frozen metric name against the constructors live in `instruments.rs`; not tripwire precedent.
#[test]
fn every_frozen_name_is_constructed() {
    let source = instruments_source();
    let missing: Vec<&str> = FROZEN_NAMES
        .iter()
        .copied()
        .filter(|name| !source.contains(&format!("\"{name}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "frozen names no longer constructed (retire the row AND its dashboard \n  consumers): {missing:?}"
    );
}
