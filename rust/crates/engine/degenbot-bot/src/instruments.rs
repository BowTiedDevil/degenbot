//! Named metric instruments for the drain path.
//!
//! One struct owns every instrument so the naming stays consistent and the
//! Prometheus families are discoverable in one place. Construction is lazy and
//! idempotent: the first observation site after [`crate::metrics`]
//! initialization builds the set from the global meter; when the `otel`
//! feature is compiled but the gate was off (or init has not run yet),
//! [`pipeline`] returns `None` and every helper no-ops — one branch per
//! observation, same discipline as `log::debug!`.
//!
//! # Cardinality
//!
//! No instrument here takes high-cardinality labels. Attributes are reserved
//! for small closed sets (`outcome`, `phase`, ...); per-path/per-pool detail
//! belongs in trace span fields.

use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::KeyValue;

/// Latency histogram bucket boundaries, in SECONDS (the instruments' unit).
///
/// The `OTel` SDK default explicit buckets for f64 histograms are seconds-scale
/// (`[0, 5, 10, 25, ...]`), so millisecond-scale drain-path operations (solve
/// ~670 ms, header→solved ~620 ms, sim ~4.5 ms) collapse entirely into the
/// first `le=5` bucket — Grafana then interpolates every quantile inside that
/// single 0–5 s bucket, rendering flat p50/p95 lines that are meaningless.
/// These boundaries give ~2.5× resolution from 100 µs through 10 s so real
/// latency distributions actually separate.
const LATENCY_BUCKETS_SECONDS: &[f64] = &[
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
    5.0, 10.0, 30.0, 60.0,
];

/// Every drain-path instrument, built from one meter.
pub struct PipelineInstruments {
    /// Header accepted → Published-stage dispatch (the epoch race; ADR-041
    /// §3.1 — submission/delivery subscribe at the Published edge, so the
    /// race ends there, not at solve). Succeeds the drain-era
    /// `block.header_to_solved` stamp retired with the stage machine.
    ///
    /// PROMETHEUS NAME COUPLING: renders as
    /// `degenbot_epoch_header_to_publish_seconds`; the Grafana headline stat
    /// and the `DegenbotEpochRaceSlow` alert query that string verbatim —
    /// rename here and the consumers together.
    header_to_publish: Histogram<f64>,
    /// ADR-041 I3: stale-epoch work items dropped loudly by the
    /// rewind-generation fail-fast (`reorg_flying_stale`). Sustained
    /// non-zero = the stage cycle chronically spills into the next epoch.
    epoch_stale_drops: Counter<u64>,
    /// ADR-041 Streaming-stage wait: first relevant log → quiesce/tombstone.
    /// The span-only `queue.age_us` attr projected to a series so the
    /// stage-cycle waterfall owns the leg without Jaeger.
    stage_streaming_age: Histogram<f64>,
    /// Header accepted → first RELEVANT log delivered (WS-feed latency —
    /// the delivery side of the pre-solve gap).
    header_to_first_log: Histogram<f64>,
    /// First → last relevant log (the in-network burst width for the
    /// block's own logs — delivery jitter inside the gap).
    log_burst: Histogram<f64>,
    /// Last relevant log → settle decision fires (debounce/quiesce wait —
    /// the pump-side hold before the drainer is engaged).
    settle_wait: Histogram<f64>,
    /// Log decode phase duration.
    log_decode: Histogram<f64>,
    /// Log apply phase duration (successful applies only).
    state_apply: Histogram<f64>,
    /// Accepted block headers.
    blocks_observed: Counter<u64>,
    /// Relevant-topic logs entering the dispatcher.
    logs_received: Counter<u64>,
    /// Every `WsEvent::Log` received by the WS pump (pre topic-filter) — the
    /// delivery-volume signal pairing with `logs_received` (relevant subset).
    ws_logs_seen: Counter<u64>,
    /// Logs applied to a registered pool.
    logs_applied: Counter<u64>,
    /// Relevant-topic logs a decoder recognized (decode success).
    logs_decoded: Counter<u64>,
    /// Relevant-topic logs that matched NO decoder (the undecoded topic0 /
    /// unknown-fork class feeding `MissedLog` desyncs).
    logs_undecoded: Counter<u64>,
    /// Relevant-topic logs that decoded but matched no registered pool (apply-miss).
    apply_missed: Counter<u64>,
    /// WAJEQP T-R1: reorg episodes entered (one per `EnterReorg`).
    reorg_windows: Counter<u64>,
    /// WAJEQP T-R1: per-pool journal restores that CHANGED state (idempotent
    /// no-ops excluded).
    reorg_unwound_pools: Counter<u64>,
    /// WAJEQP T-R1: rollback depth at episode entry (current head − reorg
    /// target, in blocks).
    reorg_depth_blocks: Histogram<f64>,
    /// WAJEQP T-R1: log events discarded by the recovery-anchor rule
    /// (`DroppedRecovery`); spikes during reorg episodes.
    reorg_recovery_dropped: Counter<u64>,
    /// forward logs admitted LATE — arrived after their block's D1
    /// tombstone and dropped un-applied via the benign late-admit path (the
    /// no-landmine ruling: counted delivery noise, never a fatal signal).
    /// Distinct from `reorg_recovery_dropped`, which counts only single-
    /// writer duplicates inside an authoritative catch-up's owned range.
    late_log_admitted: Counter<u64>,
    /// the currently-armed settle (quiesce) window in ms — fixed
    /// mode the debounce, adaptive mode the estimator's clamped EWMA
    /// projection. The §6.2 design instrument pair with
    /// `late_log.admitted`.
    quiesce_window_ms: Gauge<f64>,
    /// Header-gap / settle backfills executed.
    backfills_executed: Counter<u64>,
    /// `pool_state_head - engine clock` divergence.
    state_head_lag_blocks: Gauge<f64>,
    /// Seconds since the newest accepted `newHeads` header (pump/feed liveness).
    pump_seconds_since_header: Gauge<f64>,
    /// Seconds since the newest log applied to state (state-advance liveness).
    pump_seconds_since_apply: Gauge<f64>,
    /// `MEVBlocker` searcher pending-tx feed liveness: 1 while the WS session
    /// is up, 0 between reconnects.
    backrun_feed_connected: Gauge<f64>,
    /// Age of the newest accepted searcher event; grows while the feed is
    /// connected but silent (a parked `MEVBlocker` auction shows up here).
    backrun_feed_seconds_since_event: Gauge<f64>,
    /// Accepted pending-tx notifications (counted from sampler deltas).
    backrun_feed_frames: Counter<u64>,
    /// Ring evictions on the feed's drop-oldest channel (consumer lag).
    backrun_feed_dropped_ring: Counter<u64>,
    /// Frames dropped by parse (malformed or unknown shape; tolerated by
    /// design — counted, never fatal).
    backrun_feed_rejected_parse: Counter<u64>,
    /// Frames rejected by the feed's chain-id gate.
    backrun_feed_rejected_chain_id: Counter<u64>,
    /// WS session teardowns + re-establishments (incl. watchdog stalls).
    backrun_feed_reconnects: Counter<u64>,
    /// Frame-terminal decisions by {decision, reason} — the silent-drop
    /// killer: every drained searcher frame exits through exactly one
    /// Bucket of this counter (`no_candidate`, `reverted`, a composed
    /// `bid`, ...).
    backrun_frame_observed: Counter<u64>,
    /// Engine-`Mutex` HOLD duration for a dirty solve cycle (T2 SLOWEXEC);
    /// distinct from `solve_duration` only if the hold moves off the calling
    /// thread — today hold == cycle by design, so this is the hold metric.
    mutex_hold_duration: Histogram<f64>,
    /// Solve lock-hold duration (dirty solves only — no-op solves are gated
    /// out of the span path and the histogram alike).
    solve_duration: Histogram<f64>,
    /// Per-path solve duration (the `solve_fn` closure timing — includes
    /// the profit-envelope gate + the decomposed solver). Distinct from
    /// `solve_duration` which measures the whole dirty-carrying solve CYCLE.
    per_path_solve_duration: Histogram<f64>,
    /// Per-path profit-envelope gate evaluation time (the `path_profit_bound`
    /// call before the decomposed solver).
    per_path_gate_duration: Histogram<f64>,
    /// Solve cycles that carried dirty work.
    solves_executed: Counter<u64>,
    /// Registered solver paths (engine gauge).
    registered_paths: Gauge<f64>,
    /// host intake backlog depth, one series per role
    /// (`degenbot_fleet_intake_backlog{role=...}`). A stalled held backlog
    /// becomes observable here (T9 soak reads it).
    intake_backlog: Gauge<f64>,
    /// Worker census: one row per registered execution resource
    /// (`resource` = the `degenbot_core::worker_census` registry id, a
    /// small closed set). Rendered as `degenbot_worker_census{resource=...}`.
    worker_census: Gauge<f64>,
    /// FF-T5: the resolved fleet profile — one series,
    /// low-cardinality labels (profile / binding / oversubscribed),
    /// value 1. Rendered as
    /// `degenbot_fleet_profile{profile=...,binding=...}`; the ops
    /// alerting reads `binding="serial"` (the production alert).
    fleet_profile: Gauge<f64>,
    /// Candidates entering the simulate fan-out (per-batch sizes summed).
    candidates_found: Counter<u64>,
    /// Solver CL-hop self-corrections (input/forward clamp + output align).
    clamps_applied: Counter<u64>,
    /// Distinct failures surfaced through [`crate::telemetry::record_exception`],
    /// labeled by the closed-set `kind` taxonomy.
    errors_total: Counter<u64>,
    /// CFS throttle events observed on the process cgroup (delta per block)
    cgroup_throttled: Counter<u64>,
    /// CPU-time stolen by CFS throttling (cumulative frozen thread time)
    cgroup_throttle_time: Counter<f64>,
    /// Per-path EVM simulation duration.
    simulate_duration: Histogram<f64>,
    /// Simulation outcomes, labeled by verdict string.
    simulate_verdicts: Counter<u64>,
    /// Per-candidate gross profit (wei) at submit entry.
    dispatch_gross_profit: Histogram<f64>,
    /// Per-candidate net profit (wei) at submit entry.
    dispatch_net_profit: Histogram<f64>,
    /// Per-candidate simulated gas.
    dispatch_gas_used: Histogram<f64>,
    /// Submit outcomes, labeled (`submitted`, `skipped_dry_run`, ...).
    submit_outcomes: Counter<u64>,
    /// Candidate loop start → broadcast latency.
    submit_latency: Histogram<f64>,
    /// Cumulative confirmed net profit (wei).
    profit_realized: Counter<f64>,
    /// PRG-2: registration skips by reason (closed set; the
    /// Python-side per-pool `SkipGate` memo retired in favor of the Rust
    /// registration gate + this family). Error-class detail stays in log
    /// spans and the `[build_paths] Progress` breakdown.
    registration_skips: Counter<u64>,
    /// Cumulative un-submitted profitable-candidate net profit (wei).
    profit_missed: Counter<f64>,
    /// Monitor outcomes, labeled (`confirmed`, `expired`).
    monitor_outcomes: Counter<u64>,
    detached_in_flight: Gauge<f64>,
    /// detached stragglers DROPPED by the Q1a stale policy.
    detached_stale_dropped: Counter<u64>,
    /// detached stragglers applied to the results map.
    detached_applied: Counter<u64>,
    /// Cold-start trace: cycles the machine DEGRADED to the in-cycle arm.
    /// WFF6MM retired that arm (and its producer): the series is retained so
    /// dashboards keep a stable zero rather than a missing metric.
    detached_degraded_cycles: Counter<u64>,
    /// detached outcomes LOST to a DEAD MERGE DRAIN — a
    /// `LaneOutcome` send failed (the sidecar Receiver is gone) or an
    /// outcome was drained/counted as the panicked sidecar shut down. This
    /// is the only silent loss in the shipped posture; the counter makes it
    /// measurable and (with the sticky posture cordon) impossible to miss.
    detached_send_failed: Counter<u64>,
    /// merge-seat panics caught by the sidecar's `catch_unwind`
    /// guard (the typed failure record for a dead merge seat).
    detached_merge_panic: Counter<u64>,
    /// QTZGFL: solve cycles SHED by capacity-modulated admission (zero draw
    /// budget: nothing submitted, cursor advanced, keys retained for carry).
    detached_shed: Counter<u64>,
    /// QTZGFL: retained (carried) admission keys pruned by the retention
    /// window (`head − W`) — a starved lead's visible expiry.
    detached_leads_expired: Counter<u64>,
    /// time an acquisition waited for the core `BotState`
    /// lock, labeled by `site` (closed set from
    /// `bot_core::state_lock::crate::bot_core::state_lock::LockSite::label`) + `mode` (read|write).
    state_lock_wait: Histogram<f64>,
    /// time a guard was held after acquisition, same labels.
    state_lock_hold: Histogram<f64>,
    /// Close-out: resident set bytes of the bot process (the
    /// drift-watch signal — tick maps + revm working set grow linearly with
    /// the registry, and a container OOM-kill presents as an overnight
    /// availability failure, not a solver symptom).
    ///
    /// PROMETHEUS NAME COUPLING (pair-review flag, 2026-09-04): the `OTel`
    /// name-mapping renders this gauge (dots->underscores + `By` unit
    /// suffix) as `degenbot_process_rss_bytes`, and the Grafana panel
    /// "Process RSS - registry drift watch" queries that string verbatim —
    /// the match is by convention, not checked. If you rename here or drop
    /// the unit attribute, update the panel expression in the same commit.
    process_rss_bytes: Gauge<f64>,

    /// ADR-040: per-error-reason tally for error-outcome simulations. The
    /// `reason` label is the closed `telemetry::error_reason` set.
    sim_error_reasons: Counter<u64>,
    /// per-block log funnel (the pump samples at each header).
    /// PROMETHEUS NAME COUPLING: renders as `degenbot_epoch_logs_{seen,
    /// received,applied,ignored,block}` — the dashboard stacked-funnel
    /// panel + the block-number xychart query those verbatim; rename here
    /// and the panels together. `block` = the header number at which the
    /// ledger closed (the xychart uses it as the block-number axis).
    epoch_logs_seen: Gauge<f64>,
    epoch_logs_received: Gauge<f64>,
    epoch_logs_applied: Gauge<f64>,
    epoch_logs_ignored: Gauge<f64>,
    epoch_logs_block: Gauge<f64>,
    /// first relevant log → publish per quiesce cycle
    /// (the publish-cycle duration for the Final-integration A/B).
    publish_cycle: Histogram<f64>,
    /// Rewind frequency (one per `EnterReorg` — the
    /// stage-table Rewind row; pairs with `reorg_windows` above).
    rewind_total: Counter<u64>,
    /// Rewind open → close duration.
    rewind_duration: Histogram<f64>,
    /// ADR-040: pools currently quarantine-excluded from solve resolution.
    /// Maintained by `BotState::quarantine_pool`/`release_pool`.
    ///
    /// PROMETHEUS NAME COUPLING (ADR-040): the `OTel` name-mapping renders this
    /// gauge as `degenbot_engine_quarantined_pools`; the Grafana panel added
    /// with the metrics doc queries that string verbatim. Rename here and the
    /// panel together.
    quarantined_pools: Gauge<f64>,
}

impl PipelineInstruments {
    /// Build all instruments from a meter. Visible for tests — production
    /// callers go through [`pipeline`].
    #[must_use]
    #[expect(clippy::too_many_lines)] // one instrument per block; splitting hides the inventory
    pub fn new(meter: &Meter) -> Self {
        let instruments = Self {
            header_to_publish: meter
                .f64_histogram("degenbot.epoch.header_to_publish")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description(
                    "Header accepted to Published-stage dispatch (the epoch race; ADR-041)",
                )
                .build(),
            epoch_stale_drops: meter
                .u64_counter("degenbot.epoch.stale_drops")
                .with_description(
                    "Stale-epoch work items dropped by the I3 rewind-generation fail-fast",
                )
                .build(),
            stage_streaming_age: meter
                .f64_histogram("degenbot.stage.streaming_age")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("First relevant log to epoch quiesce (Streaming stage wait)")
                .build(),
            header_to_first_log: meter
                .f64_histogram("degenbot.block.header_to_first_log")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description(
                    "Header accepted to first relevant log delivered (WS delivery latency)",
                )
                .build(),
            log_burst: meter
                .f64_histogram("degenbot.block.log_burst")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description(
                    "First to last relevant log delivery (burst jitter inside the pre-solve gap)",
                )
                .build(),
            settle_wait: meter
                .f64_histogram("degenbot.block.settle_wait")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Last relevant log to settle decision (debounce/quiesce wait)")
                .build(),
            log_decode: meter
                .f64_histogram("degenbot.log.decode")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Log decode phase duration")
                .build(),
            state_apply: meter
                .f64_histogram("degenbot.state.apply")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Log apply phase duration (successful applies)")
                .build(),
            blocks_observed: meter
                .u64_counter("degenbot.blocks.observed")
                .with_description("Accepted block headers")
                .build(),
            reorg_windows: meter
                .u64_counter("degenbot.reorg.windows")
                .with_description("Reorg episodes entered (EnterReorg)")
                .build(),
            reorg_unwound_pools: meter
                .u64_counter("degenbot.reorg.unwound_pools")
                .with_description("Per-pool journal restores that changed state")
                .build(),
            reorg_depth_blocks: meter
                .f64_histogram("degenbot.reorg.depth_blocks")
                .with_unit("1")
                .with_boundaries(vec![1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 20.0, 33.0, 50.0, 100.0])
                .with_description(
                    "Rollback depth at reorg-episode entry (current head minus reorg target)",
                )
                .build(),
            reorg_recovery_dropped: meter
                .u64_counter("degenbot.reorg.recovery_dropped")
                .with_description(
                    "Log events discarded by the recovery-anchor rule (DroppedRecovery)",
                )
                .build(),
            quiesce_window_ms: meter
                .f64_gauge("degenbot.quiesce.window_ms")
                .with_description(
                    "Currently-armed settle (quiesce) window in ms (fixed debounce or the adaptive EWMA estimator's W)",
                )
                .build(),
            late_log_admitted: meter
                .u64_counter("degenbot.late_log.admitted")
                .with_description(
                    "Forward logs dropped via the benign late-admit path (arrived after their block's D1 tombstone)",
                )
                .build(),
            logs_received: meter
                .u64_counter("degenbot.logs.received")
                .with_description("Relevant-topic logs entering the dispatcher")
                .build(),
            logs_applied: meter
                .u64_counter("degenbot.logs.applied")
                .with_description("Logs applied to a registered pool")
                .build(),
            apply_missed: meter
                .u64_counter("degenbot.logs.apply_missed")
                .with_description("Relevant-topic logs decoded but no registered pool (apply-miss)")
                .build(),
            ws_logs_seen: meter
                .u64_counter("degenbot.ws.logs.seen")
                .with_description("Every WS log event received by the pump (pre topic-filter)")
                .build(),
            logs_decoded: meter
                .u64_counter("degenbot.logs.decoded")
                .with_description("Relevant-topic logs a decoder recognized")
                .build(),
            logs_undecoded: meter
                .u64_counter("degenbot.logs.undecoded")
                .with_description("Relevant-topic logs that matched no decoder")
                .build(),
            backfills_executed: meter
                .u64_counter("degenbot.backfills.executed")
                .with_description("Header-gap or settle backfills executed")
                .build(),
            state_head_lag_blocks: meter
                .f64_gauge("degenbot.state.head_lag_blocks")
                .with_description(
                    "pool_state_head minus engine clock (negative = pools trail the pump clock)",
                )
                .build(),
            pump_seconds_since_header: meter
                .f64_gauge("degenbot.pump.seconds_since_header")
                .with_description(
                    "Seconds since the newest accepted newHeads header (pump/feed liveness)",
                )
                .build(),
            pump_seconds_since_apply: meter
                .f64_gauge("degenbot.pump.seconds_since_apply")
                .with_description(
                    "Seconds since the newest log applied to state (state-advance liveness)",
                )
                .build(),
            backrun_feed_connected: meter
                .f64_gauge("degenbot.backrun.feed.connected")
                .with_description("MEVBlocker searcher feed WS session is up (1) or down (0)")
                .build(),
            backrun_feed_seconds_since_event: meter
                .f64_gauge("degenbot.backrun.feed.seconds_since_event")
                .with_description(
                    "Seconds since the newest accepted searcher pending-tx event",
                )
                .build(),
            backrun_feed_frames: meter
                .u64_counter("degenbot.backrun.feed.frames")
                .with_description("Accepted searcher pending-tx notifications")
                .build(),
            backrun_feed_dropped_ring: meter
                .u64_counter("degenbot.backrun.feed.dropped_ring")
                .with_description("Feed ring evictions (drop-oldest, consumer lag)")
                .build(),
            backrun_feed_rejected_parse: meter
                .u64_counter("degenbot.backrun.feed.rejected_parse")
                .with_description("Searcher frames dropped by parse (tolerated, counted)")
                .build(),
            backrun_feed_rejected_chain_id: meter
                .u64_counter("degenbot.backrun.feed.rejected_chain_id")
                .with_description("Searcher frames rejected by the feed's chain-id gate")
                .build(),
            backrun_feed_reconnects: meter
                .u64_counter("degenbot.backrun.feed.reconnects")
                .with_description("Feed WS session teardowns incl. watchdog stalls")
                .build(),
            backrun_frame_observed: meter
                .u64_counter("degenbot.backrun.frame.observed")
                .with_description(
                    "Backrun frame terminal decisions (decision/reason labels)",
                )
                .build(),
            mutex_hold_duration: meter
                .f64_histogram("degenbot.solve.mutex_hold")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Engine Mutex hold duration for a dirty solve cycle")
                .build(),
            solve_duration: meter
                .f64_histogram("degenbot.solve.duration")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Dirty-carrying solve cycle duration")
                .build(),
            per_path_solve_duration: meter
                .f64_histogram("degenbot.solve.path_duration")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Per-path solve duration (gate + decomposed solver)")
                .build(),
            per_path_gate_duration: meter
                .f64_histogram("degenbot.solve.gate_duration")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Per-path profit-envelope gate evaluation time")
                .build(),
            solves_executed: meter
                .u64_counter("degenbot.solves.executed")
                .with_description("Solve cycles that carried dirty work")
                .build(),
            registered_paths: meter
                .f64_gauge("degenbot.engine.registered_paths")
                .with_description("Registered solver paths")
                .build(),
            intake_backlog: meter
                .f64_gauge("degenbot.fleet.intake_backlog")
                .with_description(
                    "Host intake backlog depth per role (TB4QGX T7): held-but-unadmitted units; a stalled held backlog is visible here",
                )
                .build(),
            worker_census: meter
                .f64_gauge("degenbot.worker.census")
                .with_description(
                    "Worker census: declared worker/slot count per execution resource; the resource label is the census registry id",
                )
                .build(),
            fleet_profile: meter
                .f64_gauge("degenbot.fleet.profile")
                .with_description(
                    "The resolved fleet host-binding profile (FF-T5): profile / binding / oversubscribed; alert on binding=\"serial\" (the small-host tier)",
                )
                .build(),
            candidates_found: meter
                .u64_counter("degenbot.candidates.found")
                .with_description("Candidates entering the simulate fan-out")
                .build(),
            clamps_applied: meter
                .u64_counter("degenbot.solver.clamps")
                .with_description("Solver CL-hop capacity/output alignment corrections")
                .build(),
            errors_total: meter
                .u64_counter("degenbot.errors")
                .with_description("Distinct failures by closed-set kind")
                .build(),
            cgroup_throttled: meter
                .u64_counter("degenbot.cgroup.throttled")
                .with_description("CFS throttle events (nr_throttled delta) on the process cgroup")
                .build(),
            cgroup_throttle_time: meter
                .f64_counter("degenbot.cgroup.throttle_time")
                .with_unit("s")
                .with_description("Thread-time frozen by CFS throttling (throttled_usec delta)")
                .build(),
            simulate_duration: meter
                .f64_histogram("degenbot.simulate.duration")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Per-path EVM simulation duration")
                .build(),
            simulate_verdicts: meter
                .u64_counter("degenbot.simulate.verdicts")
                .with_description("Simulation outcomes by verdict")
                .build(),
            dispatch_gross_profit: meter
                .f64_histogram("degenbot.dispatch.gross_profit")
                .with_unit("wei")
                .with_description("Per-candidate gross profit at submit entry")
                .build(),
            dispatch_net_profit: meter
                .f64_histogram("degenbot.dispatch.net_profit")
                .with_unit("wei")
                .with_description("Per-candidate net profit at submit entry")
                .build(),
            dispatch_gas_used: meter
                .f64_histogram("degenbot.dispatch.gas_used")
                .with_description("Per-candidate simulated gas")
                .build(),
            submit_outcomes: meter
                .u64_counter("degenbot.submit.outcomes")
                .with_description("Submit outcomes by reason")
                .build(),
            submit_latency: meter
                .f64_histogram("degenbot.submit.latency")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Candidate loop start to broadcast")
                .build(),
            profit_realized: meter
                .f64_counter("degenbot.profit.realized")
                .with_unit("wei")
                .with_description("Cumulative confirmed net profit")
                .build(),
            profit_missed: meter
                .f64_counter("degenbot.profit.missed")
                .with_unit("wei")
                .with_description("Cumulative net profit of un-submitted candidates")
                .build(),
            monitor_outcomes: meter
                .u64_counter("degenbot.monitor.outcomes")
                .with_description("Monitor outcomes (confirmed/expired)")
                .build(),
            registration_skips: meter
                .u64_counter("degenbot.registration.skips")
                .with_description("Registration-candidate skips by closed-set reason (PRG-2)")
                .build(),
            detached_in_flight: meter
                .f64_gauge("degenbot.detached.in_flight")
                .with_description(
                    "Outstanding un-merged detached solve results (merge-pipe stragglers)",
                )
                .build(),
            detached_stale_dropped: meter
                .u64_counter("degenbot.detached.stale_dropped")
                .with_description("Detached stragglers dropped by the Q1a stale/deregister policy")
                .build(),
            detached_applied: meter
                .u64_counter("degenbot.detached.applied")
                .with_description("Detached stragglers applied to the results map")
                .build(),
            detached_degraded_cycles: meter
                .u64_counter("degenbot.detached.degraded_cycles")
                .with_description("Solve cycles DEGRADED to the in-cycle arm (in-flight cap verdict at begin)")
                .build(),
            detached_send_failed: meter
                .u64_counter("degenbot.detached.send_failed")
                .with_description("Detached outcomes lost to a dead merge drain (failed send / post-panic drain)")
                .build(),
            detached_merge_panic: meter
                .u64_counter("degenbot.detached.merge_panic")
                .with_description("Merge-seat panics caught by the sidecar guard (dead merge drain)")
                .build(),
            detached_shed: meter
                .u64_counter("degenbot.detached.shed")
                .with_description("Solve cycles SHED by capacity-modulated admission (zero draw budget)")
                .build(),
            detached_leads_expired: meter
                .u64_counter("degenbot.detached.leads_expired")
                .with_description("Retained admission keys expired by the retention window (head - W)")
                .build(),
            state_lock_wait: meter
                .f64_histogram("degenbot.state_lock.wait")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description(
                    "Time an acquisition waited for the core BotState lock (per site, mode)",
                )
                .build(),
            state_lock_hold: meter
                .f64_histogram("degenbot.state_lock.hold")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Time a guard was held after acquisition (per site, mode)")
                .build(),
            epoch_logs_seen: meter.f64_gauge("degenbot.epoch.logs_seen").build(),
            epoch_logs_received: meter.f64_gauge("degenbot.epoch.logs_received").build(),
            epoch_logs_applied: meter.f64_gauge("degenbot.epoch.logs_applied").build(),
            epoch_logs_ignored: meter.f64_gauge("degenbot.epoch.logs_ignored").build(),
            epoch_logs_block: meter.f64_gauge("degenbot.epoch.logs_block").build(),
            publish_cycle: meter
                .f64_histogram("degenbot.stage.publish_cycle")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description(
                    "First relevant log to publish, per quiesce cycle (publish-cycle duration)",
                )
                .build(),
            rewind_total: meter
                .u64_counter("degenbot.stage.rewind")
                .with_description(
                    "Rewind episodes entered (EnterReorg; the stage-table Rewind row)",
                )
                .build(),
            rewind_duration: meter
                .f64_histogram("degenbot.stage.rewind_duration")
                .with_unit("s")
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .with_description("Rewind (reorg unwind window) open to close duration")
                .build(),
            sim_error_reasons: meter
                .u64_counter("degenbot.sim.error_reason")
                .with_description("Sim-attributed failures by closed error_reason")
                .build(),
            quarantined_pools: meter
                .f64_gauge("degenbot.engine.quarantined_pools")
                .with_description(
                    "Pools currently quarantine-excluded from solve resolution (ADR-040)",
                )
                .build(),
            process_rss_bytes: meter
                .f64_gauge("degenbot.process.rss")
                .with_unit("By")
                .with_description(
                    "Resident set bytes of the bot process (drift-watch)",
                )
                .build(),
        };
        // Cold-start trace: zero-initialize the degraded-cycle counter. The
        // OTel Prometheus exporter omits an instrument that never recorded a
        // measurement, so an ABSENT series was previously indistinguishable
        // from "never degraded" (the 2026-09-11 cold soak hit exactly that:
        // `_applied_total` and `_in_flight` rendered, `degraded_cycles` did
        // not). This explicit 0 keeps the series always present, making a
        // scraped `0` mean what it says.
        instruments.detached_degraded_cycles.add(0, &[]);
        // same zero-init for the dead-drain instruments — a missing
        // `send_failed_total` series must never read as "no lost outcomes".
        instruments.detached_send_failed.add(0, &[]);
        instruments.detached_merge_panic.add(0, &[]);
        // QTZGFL: same zero-init contract for the admission counters — a
        // missing `shed_total`/`leads_expired_total` series must never read
        // as "nothing shed / nothing expired" (the 9395c481b lesson).
        instruments.detached_shed.add(0, &[]);
        instruments.detached_leads_expired.add(0, &[]);
        instruments
    }

    /// Header accepted → Published-stage dispatch (the epoch race).
    /// PROMETHEUS NAME COUPLING: renders as
    /// `degenbot_epoch_header_to_publish_seconds` — see the field note.
    pub fn observe_header_to_publish(&self, secs: f64) {
        self.header_to_publish.record(secs, &[]);
    }

    /// Streaming-stage wait: first relevant log → quiesce/tombstone.
    pub fn observe_streaming_age(&self, secs: f64) {
        self.stage_streaming_age.record(secs, &[]);
    }

    /// One stale-epoch work item dropped (I3 reorg-flying fail-fast).
    pub fn count_stale_drop(&self) {
        self.epoch_stale_drops.add(1, &[]);
    }

    /// Header accepted → first relevant log delivered (pre-solve gap phase 1).
    pub fn observe_header_to_first_log(&self, secs: f64) {
        self.header_to_first_log.record(secs, &[]);
    }

    /// First → last relevant log delivered (pre-solve gap phase 2).
    pub fn observe_log_burst(&self, secs: f64) {
        self.log_burst.record(secs, &[]);
    }

    /// Last relevant log → settle decision (pre-solve gap phase 3).
    pub fn observe_settle_wait(&self, secs: f64) {
        self.settle_wait.record(secs, &[]);
    }

    /// Log decode phase duration.
    pub fn observe_log_decode(&self, secs: f64) {
        self.log_decode.record(secs, &[]);
    }

    /// Apply phase duration for a successful apply.
    pub fn observe_state_apply(&self, secs: f64) {
        self.state_apply.record(secs, &[]);
    }

    /// One accepted header.
    pub fn count_block(&self) {
        self.blocks_observed.add(1, &[]);
    }

    /// One relevant-topic log dispatched.
    pub fn count_log_received(&self) {
        self.logs_received.add(1, &[]);
    }

    /// One successful pool apply.
    pub fn count_log_applied(&self) {
        self.logs_applied.add(1, &[]);
    }

    /// One relevant-topic log that decoded but matched no registered pool.
    pub fn count_log_apply_missed(&self) {
        self.apply_missed.add(1, &[]);
    }

    /// One reorg episode entered (`EnterReorg`) — WAJEQP T-R1.
    pub fn count_reorg_window(&self) {
        self.reorg_windows.add(1, &[]);
    }

    /// One per-pool journal restore that CHANGED state — WAJEQP T-R1.
    pub fn count_reorg_unwound_pool(&self) {
        self.reorg_unwound_pools.add(1, &[]);
    }

    /// Rollback depth at reorg-episode entry, in blocks — WAJEQP T-R1.
    pub fn observe_reorg_depth(&self, blocks: u64) {
        self.reorg_depth_blocks
            .record(f64::from(u32::try_from(blocks).unwrap_or(u32::MAX)), &[]);
    }

    /// One log event discarded by the recovery-anchor rule — WAJEQP T-R1.
    pub fn count_reorg_recovery_dropped(&self) {
        self.reorg_recovery_dropped.add(1, &[]);
    }

    /// One forward log admitted LATE (past its block's D1 tombstone) and
    /// dropped un-applied via the benign late-admit path. The
    /// counted home for settle-window delivery jitter; a sustained rate
    /// says the WS feed reordered, not that the state machine misbehaved.
    pub fn count_late_log_admitted(&self) {
        self.late_log_admitted.add(1, &[]);
    }

    /// the window the settle timers are currently armed with (ms).
    /// Observed once per settle point so the Grafana funnel gains the
    /// estimator's live posture without new scrape paths.
    #[expect(clippy::cast_precision_loss)]
    pub fn observe_quiesce_window(&self, ms: u64) {
        self.quiesce_window_ms.record(ms as f64, &[]);
    }

    /// One WS log event received by the pump (pre topic-filter).
    pub fn count_ws_log_seen(&self) {
        self.ws_logs_seen.add(1, &[]);
    }

    /// One relevant-topic log a decoder recognized.
    pub fn count_log_decoded(&self) {
        self.logs_decoded.add(1, &[]);
    }

    /// One relevant-topic log that matched no decoder (undecoded topic0).
    pub fn count_log_undecoded(&self) {
        self.logs_undecoded.add(1, &[]);
    }

    /// CFS throttle deltas observed since the previous call. Zero-record
    /// deltas are skipped: Prometheus-style exporters need no per-block
    /// sampler noise when the cgroup was not throttled.
    pub fn observe_cgroup_throttled(&self, events_delta: u64, usecs_delta: u64) {
        if events_delta == 0 && usecs_delta == 0 {
            return;
        }
        self.cgroup_throttled.add(events_delta, &[]);
        #[expect(clippy::cast_precision_loss)] // usec -> s over 1e6; float fine
        {
            self.cgroup_throttle_time.add(usecs_delta as f64 / 1e6, &[]);
        }
    }

    /// One executed backfill range.
    pub fn count_backfill(&self) {
        self.backfills_executed.add(1, &[]);
    }

    /// Signed `pool_state_head - engine_clock` divergence in blocks. Block
    /// numbers fit far below the f64 mantissa, so the `f64::from` via i32
    /// clamp loses nothing meaningful at block-chain scale.
    pub fn set_state_head_lag(&self, head_minus_clock: i64) {
        let clamped = head_minus_clock.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        self.state_head_lag_blocks
            .record(f64::from(i32::try_from(clamped).unwrap_or_default()), &[]);
    }

    /// Age (seconds) of the newest accepted header; grows on a feed stall.
    pub fn set_seconds_since_header(&self, secs: f64) {
        self.pump_seconds_since_header.record(secs, &[]);
    }

    /// Age (seconds) of the newest log applied to state; grows on a freeze.
    pub fn set_seconds_since_apply(&self, secs: f64) {
        self.pump_seconds_since_apply.record(secs, &[]);
    }

    /// Sample the `MEVBlocker` searcher feed into the instruments. The caller
    /// (the backrun driver's own tick) owns the cadence and passes counter
    /// DELTAS since its previous sample — `OTel` counters accumulate the pushes.
    /// `seconds_since_event` is `None` until the feed's first accepted event;
    /// `None` leaves the last recorded age standing rather than overwriting
    /// it with a fake 0.
    #[expect(
        clippy::too_many_arguments,
        reason = "one flat sample of a closed 7-field status snapshot"
    )]
    pub fn record_backrun_feed(
        &self,
        connected: bool,
        seconds_since_event: Option<f64>,
        frames: u64,
        dropped_ring: u64,
        rejected_parse: u64,
        rejected_chain_id: u64,
        reconnects: u64,
    ) {
        self.backrun_feed_connected
            .record(if connected { 1.0 } else { 0.0 }, &[]);
        if let Some(secs) = seconds_since_event {
            self.backrun_feed_seconds_since_event.record(secs, &[]);
        }
        self.backrun_feed_frames.add(frames, &[]);
        self.backrun_feed_dropped_ring.add(dropped_ring, &[]);
        self.backrun_feed_rejected_parse.add(rejected_parse, &[]);
        self.backrun_feed_rejected_chain_id
            .add(rejected_chain_id, &[]);
        self.backrun_feed_reconnects.add(reconnects, &[]);
    }

    /// Count one frame's terminal decision. `reason` is empty for a plain
    /// `bid`; the label vocabulary is the pipeline's closed `Decision`
    /// reason set (`no_candidate`, `v4_unsupported`, `reverted`, ...).
    pub fn count_backrun_frame(&self, decision: &str, reason: &str) {
        self.backrun_frame_observed.add(
            1,
            &[
                KeyValue::new("decision", decision.to_string()),
                KeyValue::new("reason", reason.to_string()),
            ],
        );
    }

    /// Engine-`Mutex` hold duration for a dirty solve cycle (T2 instrument).
    /// `arm` is the cycle-span vocabulary (`detached` | `in_cycle` |
    /// `skipped_empty` | `shed`); under the detached stance `in_cycle` IS the DEGRADED
    /// population (the cap verdict the `degenbot.detached.degraded_cycles`
    /// counter tallies), so its tail is separable from the detached arm's.
    pub fn observe_mutex_hold_duration(&self, secs: f64, arm: &'static str) {
        self.mutex_hold_duration
            .record(secs, &[KeyValue::new("arm", arm)]);
    }

    /// One dirty-carrying solve cycle's duration (same `arm` attribution as
    /// the hold above — the pairing is what answers "what does degradation
    /// cost" per cycle).
    pub fn observe_solve_duration(&self, secs: f64, arm: &'static str) {
        self.solve_duration
            .record(secs, &[KeyValue::new("arm", arm)]);
    }

    /// One per-path solve closure's duration (gate + decomposed solver).
    pub fn observe_per_path_solve_duration(&self, secs: f64) {
        self.per_path_solve_duration.record(secs, &[]);
    }

    /// One per-path profit-envelope gate evaluation time.
    pub fn observe_per_path_gate_duration(&self, secs: f64) {
        self.per_path_gate_duration.record(secs, &[]);
    }

    /// One solve cycle that carried dirty work.
    pub fn count_solves_executed(&self) {
        self.solves_executed.add(1, &[]);
    }

    /// Current registered-path count (engine gauge).
    pub fn set_registered_paths(&self, count: u64) {
        self.registered_paths
            .record(f64::from(u32::try_from(count).unwrap_or(u32::MAX)), &[]);
    }

    /// the host intake backlog depth for `role`
    /// (`degenbot_fleet_intake_backlog{role=...}`).
    pub fn set_intake_backlog(&self, role: &str, depth: u64) {
        self.intake_backlog.record(
            f64::from(u32::try_from(depth).unwrap_or(u32::MAX)),
            &[KeyValue::new("role", role.to_owned())],
        );
    }

    /// one census row — declared workers/slots of `resource`.
    /// `resource` is a small closed set: the census registry ids.
    pub fn set_worker_census(&self, resource: &str, workers: f64) {
        self.worker_census
            .record(workers, &[KeyValue::new("resource", resource.to_owned())]);
    }

    /// FF-T5: the resolved fleet profile — one series,
    /// value 1, the labels carry the tier.
    pub fn set_fleet_profile(
        &self,
        summary: &crate::arb_engine::fleet_status::FleetProfileSummary,
    ) {
        self.fleet_profile.record(
            1.0,
            &[
                KeyValue::new("profile", summary.profile),
                KeyValue::new("binding", summary.binding),
                KeyValue::new(
                    "oversubscribed",
                    if summary.oversubscribed {
                        "true"
                    } else {
                        "false"
                    },
                ),
            ],
        );
    }

    /// A batch of `n` candidates entered the simulate fan-out.
    pub fn count_candidates_found(&self, n: u64) {
        self.candidates_found.add(n, &[]);
    }
    /// Count one solver CL-hop correction (input/forward clamp or output align).
    pub fn count_clamp(&self) {
        self.clamps_applied.add(1, &[]);
    }

    /// Count one distinct failure of the given closed-set [`kind`](crate::telemetry::error_kind).
    pub fn count_error(&self, kind: &'static str) {
        self.errors_total.add(1, &[KeyValue::new("kind", kind)]);
    }

    /// One per-path simulation completed.
    pub fn observe_simulate_duration(&self, secs: f64) {
        self.simulate_duration.record(secs, &[]);
    }

    /// One simulation outcome; `verdict` is a small closed set
    /// (`profitable`, `not_profitable`, `error`, ...).
    pub fn count_simulate_verdict(&self, verdict: &str) {
        self.simulate_verdicts
            .add(1, &[KeyValue::new("outcome", verdict.to_owned())]);
    }

    /// One candidate's economics at submit entry (wei as f64 — dashboards
    /// chart magnitudes, not exact wei).
    pub fn observe_dispatch_profits(&self, gross_wei: f64, net_wei: f64) {
        self.dispatch_gross_profit.record(gross_wei, &[]);
        self.dispatch_net_profit.record(net_wei, &[]);
    }

    /// One candidate's simulated gas.
    #[expect(clippy::cast_precision_loss)]
    pub fn observe_dispatch_gas(&self, gas: u64) {
        self.dispatch_gas_used
            .record(gas.min(u64::from(u32::MAX)) as f64, &[]);
    }

    /// One submit outcome; `outcome` is a small closed set
    /// (`submitted`, `skipped_pools_claimed`, `skipped_dry_run`, ...).
    pub fn count_submit_outcome(&self, outcome: &str) {
        self.submit_outcomes
            .add(1, &[KeyValue::new("outcome", outcome.to_owned())]);
    }

    /// PRG-2: one registration-candidate skip; `reason` is a small
    /// closed set (`v4-admission`, `path-cap`, `dup`, `pool-build-error`,
    /// `engine-reject`, `register-fail`, ...). The live registration-gate
    /// table refuses immutable V4 admission facts pre-RPC, so this family
    /// carries the residual (transient/config) skips. Per-error-class
    /// detail belongs in log spans, not label cardinality.
    pub fn count_registration_skip(&self, reason: &str) {
        self.registration_skips
            .add(1, &[KeyValue::new("reason", reason.to_owned())]);
    }

    /// Candidate loop start → broadcast latency.
    pub fn observe_submit_latency(&self, secs: f64) {
        self.submit_latency.record(secs, &[]);
    }

    /// Add confirmed net profit (wei).
    pub fn add_profit_realized(&self, wei: f64) {
        self.profit_realized.add(wei, &[]);
    }

    /// Add un-submitted candidate net profit (wei).
    pub fn add_profit_missed(&self, wei: f64) {
        self.profit_missed.add(wei, &[]);
    }

    /// One monitor outcome (`confirmed`, `expired`).
    pub fn count_monitor_outcome(&self, outcome: &str) {
        self.monitor_outcomes
            .add(1, &[KeyValue::new("outcome", outcome.to_owned())]);
    }

    /// detached merge-pipe in-flight depth (stragglers sent
    /// but not yet applied/dropped). Sampled at enqueue + per disposition.
    pub fn set_detached_in_flight(&self, count: u64) {
        self.detached_in_flight
            .record(f64::from(u32::try_from(count).unwrap_or(u32::MAX)), &[]);
    }

    /// One detached straggler DROPPED by the Q1a stale/deregister gate.
    pub fn count_detached_stale_dropped(&self) {
        self.detached_stale_dropped.add(1, &[]);
    }

    /// One detached outcome LOST to a dead merge drain: a failed
    /// pipe send or an outcome drained as the panicked sidecar shut down.
    pub fn count_detached_send_failed(&self) {
        self.detached_send_failed.add(1, &[]);
    }

    /// One merge-seat panic caught by the sidecar guard.
    pub fn count_detached_merge_panic(&self) {
        self.detached_merge_panic.add(1, &[]);
    }

    /// QTZGFL: one solve cycle SHED by capacity-modulated admission.
    pub fn count_detached_shed(&self) {
        self.detached_shed.add(1, &[]);
    }

    /// QTZGFL: `n` retained admission keys expired by the retention window.
    pub fn count_detached_leads_expired(&self, n: u64) {
        self.detached_leads_expired.add(n, &[]);
    }

    /// One detached straggler applied to the results map.
    pub fn count_detached_applied(&self) {
        self.detached_applied.add(1, &[]);
    }

    /// One solve cycle DEGRADED to the in-cycle arm (the cap verdict at begin).
    /// no producer remains (the in-cycle arm is deleted); kept so the
    /// zero-init + broken-pipe it exercises stay tested.
    pub fn count_detached_degraded_cycle(&self) {
        self.detached_degraded_cycles.add(1, &[]);
    }

    /// one state-lock acquisition wait. `site` is a small
    /// closed set (see `bot_core::state_lock::crate::bot_core::state_lock::LockSite::label`); `mode` is
    /// `read` | `write`.
    pub fn observe_state_lock_wait(&self, site: &str, mode: &str, secs: f64) {
        self.state_lock_wait.record(
            secs,
            &[
                KeyValue::new("site", site.to_owned()),
                KeyValue::new("mode", mode.to_owned()),
            ],
        );
    }

    /// one post-acquisition guard hold (same labels).
    pub fn observe_state_lock_hold(&self, site: &str, mode: &str, secs: f64) {
        self.state_lock_hold.record(
            secs,
            &[
                KeyValue::new("site", site.to_owned()),
                KeyValue::new("mode", mode.to_owned()),
            ],
        );
    }

    /// per-block log funnel — the pump's header-epilogue snapshot
    /// (degenbot.epoch.logs_*; see the field note for the coupling).
    /// `closing_block` = the header number at which the ledger closed.
    #[expect(clippy::cast_precision_loss)]
    pub fn observe_epoch_logs(
        &self,
        seen: u64,
        received: u64,
        applied: u64,
        ignored: u64,
        closing_block: u64,
    ) {
        self.epoch_logs_seen.record(seen as f64, &[]);
        self.epoch_logs_received.record(received as f64, &[]);
        self.epoch_logs_applied.record(applied as f64, &[]);
        self.epoch_logs_ignored.record(ignored as f64, &[]);
        self.epoch_logs_block.record(closing_block as f64, &[]);
    }

    /// one publish-cycle duration (first relevant log
    /// → publish).
    pub fn observe_publish_cycle(&self, secs: f64) {
        self.publish_cycle.record(secs, &[]);
    }

    /// one Rewind entered (`EnterReorg`).
    pub fn count_rewind(&self) {
        self.rewind_total.add(1, &[]);
    }

    /// one Rewind open→close duration.
    pub fn observe_rewind_duration(&self, secs: f64) {
        self.rewind_duration.record(secs, &[]);
    }

    /// ADR-040: one error-outcome sim tallied by its closed reason.
    pub fn count_sim_error_reason(&self, reason: &str) {
        self.sim_error_reasons
            .add(1, &[KeyValue::new("reason", reason.to_owned())]);
    }

    /// ADR-040: quarantine depth after a quarantine/release transition.
    #[expect(clippy::cast_precision_loss)]
    pub fn set_quarantined_pools(&self, count: usize) {
        self.quarantined_pools.record(count as f64, &[]);
    }

    /// Close-out: current resident set bytes (drift-watch).
    #[expect(clippy::cast_precision_loss)]
    pub fn set_process_rss_bytes(&self, bytes: u64) {
        self.process_rss_bytes.record(bytes as f64, &[]);
    }
}

/// Read the process resident set size in bytes from `/proc/self/statm`
/// (second field: resident pages; page size classically 4096 — the runs
/// this gauges are Linux `x86_64`). `None` when statm is unavailable or
/// unexpected (Windows/CI sandboxes) — callers must no-op on None.
#[must_use]
pub(crate) fn read_process_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    parse_statm_resident(&statm, 4096)
}

/// Pure parser for the statm encoding (testable without /proc).
#[must_use]
pub(crate) fn parse_statm_resident(statm: &str, page_bytes: u64) -> Option<u64> {
    let resident_pages = statm.split_whitespace().nth(1)?;
    let pages: u64 = resident_pages.parse().ok()?;
    Some(pages.saturating_mul(page_bytes))
}

#[cfg(test)]
mod tests {
    use super::parse_statm_resident;

    #[test]
    #[expect(clippy::expect_used)]
    fn statm_parser_reads_resident_pages() {
        // /proc/self/statm: size resident shared text lib data dt (pages)
        let rss = parse_statm_resident("54321 21000 1234 100 0 7000 0", 4096)
            .expect("two-page-statm parses");
        assert_eq!(rss, 21_000 * 4096);
    }

    #[test]
    fn statm_parser_is_none_on_garbage() {
        assert!(parse_statm_resident("", 4096).is_none());
        assert!(parse_statm_resident("only", 4096).is_none());
        assert!(parse_statm_resident("12 abc 3", 4096).is_none());
    }
}

static PIPELINE: OnceLock<Option<PipelineInstruments>> = OnceLock::new();

/// The process-wide instrument set, or `None` while metrics are disabled
/// (otel feature off is compiled out entirely; gate off / not yet initialized
/// lands here). Idempotent — cheap to call per observation.
#[must_use]
pub fn pipeline() -> Option<&'static PipelineInstruments> {
    PIPELINE
        .get_or_init(|| {
            // NOTE: do NOT "touch" an instrument with a marker attribute here.
            // A previous version added blocks_observed.add(0, {init:"true"}) to
            // make an empty scrape distinguishable from "never registered" —
            // but attributes are LABELS, so that touch created a permanent,
            // frozen-at-zero SECOND series (`degenbot_blocks_observed_total{
            // init="true"}`) that Prometheus alerts matching on rate()==0 fire
            // against forever (observed live 2026-08-22: DegenbotHeaderStall
            // stuck FIRING while the real series advanced). Empty-vs-absent
            // scrapes are already distinguishable via target_info.
            crate::metrics::try_global_meter().map(|meter| {
                // the census registry re-fires this hook on every
                // registration, so the scrape always reflects the table —
                // including lazily-booted resources registered after the
                // boot dump.
                degenbot_core::worker_census::set_export_hook(export_worker_census);
                let instruments = PipelineInstruments::new(&meter);
                // FF-T5: if the stamp installed BEFORE the
                // pipeline, record the fleet profile now (the install
                // path re-fires through note_fleet_profile when the
                // pipeline came first).
                if let Some(summary) = crate::arb_engine::fleet_status::fleet_profile_summary() {
                    instruments.set_fleet_profile(&summary);
                }
                instruments
            })
        })
        .as_ref()
}

/// Production census exporter — installed as the
/// [`degenbot_core::worker_census`] export hook at instruments init.
/// No-op while the pipeline is un-built (non-otel builds / gate off).
fn export_worker_census(entries: &[degenbot_core::worker_census::WorkerCensusEntry]) {
    if let Some(p) = pipeline() {
        export_worker_census_with(p, entries);
    }
}

/// FF-T5: record the resolved fleet profile (the stamp-install
/// site calls this; the pipeline may not exist yet — the build path
/// below re-reads the summary so both orders land one series). No-op
/// while the pipeline is un-built (non-otel builds / gate off).
pub fn note_fleet_profile(summary: &crate::arb_engine::fleet_status::FleetProfileSummary) {
    if let Some(p) = pipeline() {
        p.set_fleet_profile(summary);
    }
}

/// Exporter body (test seam): one gauge row per census entry.
fn export_worker_census_with(
    p: &PipelineInstruments,
    entries: &[degenbot_core::worker_census::WorkerCensusEntry],
) {
    for e in entries {
        p.set_worker_census(e.resource, census_count_f64(e.count));
    }
}

#[expect(clippy::cast_precision_loss)]
fn census_count_f64(n: usize) -> f64 {
    n as f64
}

#[cfg(test)]
#[expect(clippy::expect_used)] // metric contract asserts loudly
mod kind_tests {
    use opentelemetry::metrics::MeterProvider as _;

    use crate::instruments::{export_worker_census_with, PipelineInstruments};
    use crate::telemetry::error_kind;
    use std::collections::HashSet;

    /// True when `line` is the Prometheus sample line for exactly `name` —
    /// the name followed by a label brace or the value whitespace, never a
    /// longer family such as `<name>_total` (the doubled-suffix bug this
    /// predicate was tightened to catch: the old `starts_with` matched
    /// `name + "_total"` and masked it).
    fn series_line_matches(line: &str, name: &str) -> bool {
        line.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
    }

    /// The taxonomy is a compile-time closed set: the consts are unique and
    /// are the only values the `kind` label may take.
    #[test]
    fn error_kinds_are_unique() {
        let kinds = [
            error_kind::WS_COMPLETENESS,
            error_kind::SIM_FAILURE,
            error_kind::SUBMIT_FAILURE,
            error_kind::MONITOR_FAILURE,
            error_kind::VERIFY_MISMATCH,
            error_kind::DRAIN_STALL,
            error_kind::DRAIN_DEAD,
            error_kind::LATE_LOG,
        ];
        let unique: HashSet<&str> = kinds.iter().copied().collect();
        assert_eq!(unique.len(), kinds.len(), "duplicate failure kind");
    }

    /// The epoch race (ADR-041): `degenbot.epoch.header_to_publish` renders
    /// via the Prometheus exposition, the I3 stale-drop counter and the
    /// Streaming-age projection are scrapeable, and the retired drain-era
    /// `degenbot.block.header_to_solved` family is GONE (hard cutover — a
    /// lingering family would keep dashboards pointed at the solve endpoint
    /// instead of the Published edge).
    #[test]
    #[expect(clippy::expect_used)]
    fn epoch_race_renders_and_retired_family_gone() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_race"));
        instruments.observe_header_to_publish(0.62);
        instruments.count_stale_drop();
        instruments.observe_streaming_age(0.35);
        let text = crate::metrics::render(&registry);
        for family in [
            "degenbot_epoch_header_to_publish_seconds_bucket",
            "degenbot_epoch_stale_drops_total",
            "degenbot_stage_streaming_age_seconds_bucket",
        ] {
            assert!(
                text.contains(family),
                "epoch-race family missing from exposition: {family}"
            );
        }
        assert!(
            !text.contains("header_to_solved"),
            "retired drain-era race family still present in exposition"
        );
        drop(provider);
    }

    /// the benign late-admit counter renders through the Prometheus
    /// exposition — every late forward dropped past its block's D1 tombstone
    /// is counted here (never surfaced as a structural failure).
    #[test]
    #[expect(clippy::expect_used)]
    fn late_log_admitted_counter_renders() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_late"));
        instruments.count_late_log_admitted();
        instruments.count_late_log_admitted();
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_late_log_admitted_total"))
            .expect("late_log.admitted family missing from exposition");
        assert!(line.ends_with('2'), "{line} != 2");
        drop(provider);
    }

    /// per-block log funnel gauges (degenbot.epoch.logs_*) render
    /// the EXACT per-epoch values the pump snapshots at each header; the
    /// dashboard stacks them as the per-block funnel bars.
    #[test]
    #[expect(clippy::expect_used)]
    fn epoch_log_funnel_gauges_are_scrapeable() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_funnel"));
        instruments.observe_epoch_logs(120, 40, 30, 2, 25_934_048);
        let text = crate::metrics::render(&registry);
        for (family, value) in [
            ("degenbot_epoch_logs_seen", 120),
            ("degenbot_epoch_logs_received", 40),
            ("degenbot_epoch_logs_applied", 30),
            ("degenbot_epoch_logs_ignored", 2),
            ("degenbot_epoch_logs_block", 25_934_048),
        ] {
            let line = text.lines().find(|l| l.starts_with(family)).expect(family);
            assert!(line.ends_with(&value.to_string()), "{line} != {value}");
        }
        drop(provider);
    }

    /// Fix 3 (VPD5ZH follow-up): the 10.0s top bucket collapsed every
    /// solve over 10s into one cylinder, hiding the 90s outliers that
    /// motivated the CPU-budget fix. The tail bounds are contract, not
    /// tuning.
    #[test]
    fn latency_histograms_render_post_10s_buckets() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        // A 45s sample must land BETWEEN the new tail bounds, not fuse into
        // the old +inf-only tail: le=30 must separate it from le=60.
        instruments.observe_solve_duration(45.0, "in_cycle");
        let text = crate::metrics::render(&registry);
        for bound in ["10", "30", "60"] {
            assert!(
                text.contains(&format!("le=\"{bound}\"")),
                "missing bucket boundary le={bound} in rendered histogram"
            );
        }
    }

    /// Acceptance: `degenbot.cgroup.throttled` is SCRAPEABLE as a monotonic
    /// counter - the kernel counters that identified the 10s-bucket root
    /// cause must be visible on the dashboard without shell access.
    #[test]
    fn cgroup_throttled_is_scrapeable() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        instruments.observe_cgroup_throttled(3, 900_000);
        instruments.observe_cgroup_throttled(1, 7);
        let text = crate::metrics::render(&registry);
        assert!(
            text.contains("degenbot_cgroup_throttled_total"),
            "throttle-event counter missing"
        );
        assert!(
            text.contains("degenbot_cgroup_throttle_time_seconds_total"),
            "throttled-cpu-time counter missing"
        );
    }

    /// the worker-census gauge renders as `degenbot_worker_census`
    /// with the stable `resource` label (one row per resource).
    #[test]
    fn worker_census_gauge_is_scrapeable_by_resource() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        instruments.set_worker_census("io_runtime_workers", 2.0);
        instruments.set_worker_census("fleet_solver_slots", 6.0);
        let text = crate::metrics::render(&registry);
        assert!(
            text.contains("degenbot_worker_census"),
            "worker-census family missing from exposition: {text}"
        );
        assert!(text.contains("resource=\"io_runtime_workers\""));
        assert!(text.contains("resource=\"fleet_solver_slots\""));
        drop(provider);
    }

    /// the REGISTER→EXPORT round trip through the production helper:
    /// a registered census entry lands on the scrape with its count.
    #[test]
    fn worker_census_register_export_round_trip_reaches_the_scrape() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        degenbot_core::worker_census::register(degenbot_core::worker_census::WorkerCensusEntry {
            resource: "census-roundtrip",
            kind: "test probe",
            count: 3,
            thread_name: "census-roundtrip-{n}",
            sizing: "test sizing rule",
            binding: "logical",
        });
        export_worker_census_with(&instruments, &degenbot_core::worker_census::snapshot());
        let text = crate::metrics::render(&registry);
        assert!(
            text.contains("degenbot_worker_census"),
            "missing family: {text}"
        );
        assert!(
            text.contains("resource=\"census-roundtrip\""),
            "missing row: {text}"
        );
        drop(provider);
    }

    /// ADR-040: the quarantine depth gauge renders under its coupled
    /// Prometheus name (see the field's PROMETHEUS NAME COUPLING note).
    #[test]
    fn quarantined_pools_gauge_is_scrapeable() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        instruments.set_quarantined_pools(3);
        let text = crate::metrics::render(&registry);
        assert!(
            text.contains("degenbot_engine_quarantined_pools"),
            "quarantined-pools gauge missing from exposition: {text}"
        );
        drop(provider);
    }

    /// Acceptance: `degenbot.errors{kind=...}` is SCRAPEABLE — the counter
    /// family renders in the Prometheus exposition format with the closed-set
    /// kind label, via the production reader seam (`build_prometheus_provider`).
    #[test]
    fn count_error_is_scrapeable_by_kind() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test"));
        instruments.count_error(error_kind::SIM_FAILURE);
        instruments.count_error(error_kind::WS_COMPLETENESS);

        let text = crate::metrics::render(&registry);
        assert!(
            text.contains("degenbot_errors_total"),
            "errors family missing from exposition:\n{text}"
        );
        assert!(text.contains("kind=\"sim_failure\""));
        assert!(text.contains("kind=\"ws_completeness\""));
        // Provider stays alive to the end of the test (readers hold no strong ref).
        drop(provider);
    }

    /// WS-log pipeline counters for the desync visibility work (2BOI2V):
    /// "degenbot.ws.logs.seen" (every WS log event), "degenbot.logs.decoded"
    /// (decoder matched), "degenbot.logs.undecoded" (relevant-topic decode
    /// miss), "degenbot.solver.verify.blocks" (published blocks judged).
    /// These four families answer "did the log arrive, was it decoded, and
    /// was the solver-state check reached" from the /metrics scrape alone.
    #[test]
    fn ws_pipeline_counter_families_are_scrapeable() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_ws"));
        instruments.count_ws_log_seen();
        instruments.count_log_decoded();
        instruments.count_log_undecoded();
        let text = crate::metrics::render(&registry);
        for family in [
            "degenbot_ws_logs_seen_total",
            "degenbot_logs_decoded_total",
            "degenbot_logs_undecoded_total",
        ] {
            assert!(
                text.contains(family),
                "WS pipeline counter family missing from exposition: {family} (got {text})"
            );
        }
        drop(provider);
    }

    /// Acceptance: latency histograms carry MILLISECOND-scale explicit
    /// boundaries, so a ~0.67 s solve actually separates across buckets.
    /// With the `OTel` SDK default seconds buckets (le=5, 10, 25, ...) every
    /// drain-path observation collapses into a single le=5 bucket and
    /// Grafana interpolates flat, meaningless p50/p95 lines. This guards
    /// that regression: if the boundaries are dropped, le="0.5" / le="1"
    /// vanish from the exposition (the defaults are 5/10/25/...).
    #[test]
    fn latency_histograms_emit_millisecond_buckets() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_lat"));
        // A realistic solve: ~0.67 s. Must land BETWEEN 0.5 and 1.0 buckets.
        instruments.observe_solve_duration(0.67, "detached");
        let text = crate::metrics::render(&registry);
        // Assert ms-scale le values are present (explicit boundaries replaced
        // the 5s-first default bucket set).
        for le in ["0.001", "0.5", "1", "0.01"] {
            assert!(
                text.contains(&format!("le=\"{le}\"")),
                "latency histogram missing millisecond bucket le={le} (default coarse buckets in use?):\n{text}"
            );
        }
        // Sanity: solve_duration family is scrapeable.
        assert!(text.contains("degenbot_solve_duration_seconds_bucket"));
        drop(provider);
    }

    /// Cold-start trace (degraded-state measurement): the degraded-cycle
    /// counter must render BEFORE it ever fires. The 2026-09-11 cold soak
    /// scraped `/metrics` for 24 min and never saw
    /// `degenbot_detached_degraded_cycles_total` while its siblings
    /// (`_applied_total`, `_in_flight`) rendered — a never-measured `OTel`
    /// instrument is omitted from the exposition, so "never degraded" and
    /// "not exported" were indistinguishable. Zero-initializing the counter
    /// makes a missing series impossible and a `0` scrape meaningful.
    #[test]
    #[expect(clippy::expect_used)]
    fn degraded_cycle_counter_renders_before_any_degradation() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_degraded_zero"));
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_detached_degraded_cycles_total"))
            .expect("the degraded counter must be exported at 0 (a missing series reads as 'never degraded')");
        assert!(line.ends_with(" 0"), "{line} != 0");
        instruments.count_detached_degraded_cycle();
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_detached_degraded_cycles_total"))
            .expect("the counter stays exported after firing");
        assert!(line.ends_with(" 1"), "{line} != 1");
        drop(provider);
    }

    /// the dead-merge-drain counter must render BEFORE it ever
    /// fires — a missing series would read as "no lost outcomes" while a
    /// dead pipe silently drops every later result. Mirrors the
    /// degraded-cycle zero-init contract (9395c481b).
    #[test]
    #[expect(clippy::expect_used)]
    fn detached_send_failed_counter_renders_before_any_loss() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_send_failed_zero"));
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_detached_send_failed_total"))
            .expect("the send-failed counter must be exported at 0 (a missing series reads as 'no lost outcomes')");
        assert!(line.ends_with(" 0"), "{line} != 0");
        instruments.count_detached_send_failed();
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_detached_send_failed_total"))
            .expect("the counter stays exported after firing");
        assert!(line.ends_with(" 1"), "{line} != 1");
        drop(provider);
    }

    /// QTZGFL: the admission counters must render BEFORE they ever fire — a
    /// missing `shed_total`/`leads_expired_total` series would read as
    /// "nothing shed / nothing expired" (the 9395c481b zero-init lesson).
    #[test]
    #[expect(clippy::expect_used)]
    fn detached_admission_counters_render_before_any_event() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_admission_zero"));
        let text = crate::metrics::render(&registry);
        for name in [
            "degenbot_detached_shed_total",
            "degenbot_detached_leads_expired_total",
        ] {
            let line = text
                .lines()
                .find(|l| series_line_matches(l, name))
                .expect("the admission counter must be exported at 0");
            assert!(line.ends_with(" 0"), "{line} != 0");
        }
        instruments.count_detached_shed();
        instruments.count_detached_leads_expired(3);
        let text = crate::metrics::render(&registry);
        let shed = text
            .lines()
            .find(|l| series_line_matches(l, "degenbot_detached_shed_total"))
            .expect("the shed series stays exported after firing");
        assert!(shed.ends_with(" 1"), "{shed} != 1");
        let expired = text
            .lines()
            .find(|l| series_line_matches(l, "degenbot_detached_leads_expired_total"))
            .expect("the leads-expired series stays exported after firing");
        assert!(expired.ends_with(" 3"), "{expired} != 3");
        drop(provider);
    }

    /// Cold-start trace (degraded-state measurement, impact half): the solve
    /// latency histograms carry the dispatch arm, so the DEGRADED population's
    /// tail — `in_cycle` under the detached stance, the cap verdict the
    /// `degenbot.detached.degraded_cycles` counter tallies — is separable from
    /// the detached arm's in Prometheus. Values are the cycle-span vocabulary
    /// (`detached` | `in_cycle` | `skipped_empty` | `shed`).
    #[test]
    #[expect(clippy::expect_used)]
    fn solve_latency_histograms_carry_the_arm_label() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_arm_label"));
        instruments.observe_solve_duration(0.67, "in_cycle");
        instruments.observe_mutex_hold_duration(0.9, "detached");
        let text = crate::metrics::render(&registry);
        // The exposition carries scope labels too (otel_scope_name), so
        // match family + arm + value instead of a literal whole-label string.
        let mut attributed = 0;
        for line in text.lines() {
            let want_arm = if line.starts_with("degenbot_solve_duration_seconds_count") {
                Some("in_cycle")
            } else if line.starts_with("degenbot_solve_mutex_hold_seconds_count") {
                Some("detached")
            } else {
                None
            };
            if let Some(arm) = want_arm {
                if line.contains(&format!("arm=\"{arm}\"")) && line.ends_with(" 1") {
                    attributed += 1;
                }
            }
        }
        assert_eq!(
            attributed, 2,
            "both solve-latency histograms must attribute their sample to the cycle's arm ({text})"
        );
        drop(provider);
    }

    /// the fleet intake backlog gauge renders with its role
    /// label and the live depth — a stalled held backlog must be observable
    /// (T9 reads this series).
    #[test]
    #[expect(clippy::expect_used)]
    fn fleet_intake_backlog_gauge_renders_with_its_role_label() {
        let (provider, registry) =
            crate::metrics::build_prometheus_provider().expect("prometheus provider build");
        let instruments = PipelineInstruments::new(&provider.meter("test_intake_backlog"));
        instruments.set_intake_backlog("Solver", 7);
        let text = crate::metrics::render(&registry);
        let line = text
            .lines()
            .find(|l| l.starts_with("degenbot_fleet_intake_backlog"))
            .expect("the intake backlog gauge must be exported");
        assert!(
            line.contains("role=\"Solver\"") && line.ends_with(" 7"),
            "gauge line: {line}"
        );
        drop(provider);
    }
}
