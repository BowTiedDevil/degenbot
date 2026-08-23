//! Named metric instruments for the drain path (T2/T3 of epic RMH23E).
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

/// Every drain-path instrument, built from one meter.
pub struct PipelineInstruments {
    /// Header accepted → solve completed (the race number).
    header_to_solved: Histogram<f64>,
    /// Time a `DrainWork` item spent queued before the drainer picked it up.
    drain_queue_wait: Histogram<f64>,
    /// Log decode phase duration.
    log_decode: Histogram<f64>,
    /// Log apply phase duration (successful applies only).
    state_apply: Histogram<f64>,
    /// Accepted block headers.
    blocks_observed: Counter<u64>,
    /// Relevant-topic logs entering the dispatcher.
    logs_received: Counter<u64>,
    /// Logs applied to a registered pool.
    logs_applied: Counter<u64>,
    /// Header-gap / settle backfills executed.
    backfills_executed: Counter<u64>,
    /// Drain FIFO depth at dispatch time (approximate backlog signal).
    drain_queue_depth: Gauge<f64>,
    /// `pool_state_head - engine clock` divergence (the freeze signature).
    state_head_lag_blocks: Gauge<f64>,
    /// Solve lock-hold duration (dirty solves only — no-op solves are gated
    /// out of the span path and the histogram alike).
    solve_duration: Histogram<f64>,
    /// Solve cycles that carried dirty work.
    solves_executed: Counter<u64>,
    /// Registered solver paths (engine gauge).
    registered_paths: Gauge<f64>,
    /// Candidates entering the simulate fan-out (per-batch sizes summed).
    candidates_found: Counter<u64>,
    /// Solver CL-hop self-corrections (input/forward clamp + output align).
    clamps_applied: Counter<u64>,
    /// Distinct failures surfaced through [`crate::telemetry::record_exception`],
    /// labeled by the closed-set `kind` taxonomy.
    errors_total: Counter<u64>,
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
    /// Cumulative un-submitted profitable-candidate net profit (wei).
    profit_missed: Counter<f64>,
    /// Monitor outcomes, labeled (`confirmed`, `expired`).
    monitor_outcomes: Counter<u64>,
}

impl PipelineInstruments {
    /// Build all instruments from a meter. Visible for tests — production
    /// callers go through [`pipeline`].
    #[must_use]
    #[expect(clippy::too_many_lines)] // one instrument per block; splitting hides the inventory
    pub fn new(meter: &Meter) -> Self {
        Self {
            header_to_solved: meter
                .f64_histogram("degenbot.block.header_to_solved")
                .with_unit("s")
                .with_description("Header accepted to solve completed")
                .build(),
            drain_queue_wait: meter
                .f64_histogram("degenbot.drain.queue_wait")
                .with_unit("s")
                .with_description("DrainWork queue time before the drainer picks it up")
                .build(),
            log_decode: meter
                .f64_histogram("degenbot.log.decode")
                .with_unit("s")
                .with_description("Log decode phase duration")
                .build(),
            state_apply: meter
                .f64_histogram("degenbot.state.apply")
                .with_unit("s")
                .with_description("Log apply phase duration (successful applies)")
                .build(),
            blocks_observed: meter
                .u64_counter("degenbot.blocks.observed")
                .with_description("Accepted block headers")
                .build(),
            logs_received: meter
                .u64_counter("degenbot.logs.received")
                .with_description("Relevant-topic logs entering the dispatcher")
                .build(),
            logs_applied: meter
                .u64_counter("degenbot.logs.applied")
                .with_description("Logs applied to a registered pool")
                .build(),
            backfills_executed: meter
                .u64_counter("degenbot.backfills.executed")
                .with_description("Header-gap or settle backfills executed")
                .build(),
            drain_queue_depth: meter
                .f64_gauge("degenbot.drain.queue_depth")
                .with_description("Drain FIFO depth at dispatch time")
                .build(),
            state_head_lag_blocks: meter
                .f64_gauge("degenbot.state.head_lag_blocks")
                .with_description("pool_state_head minus engine clock (freeze signature)")
                .build(),
            solve_duration: meter
                .f64_histogram("degenbot.solve.duration")
                .with_unit("s")
                .with_description("Dirty-carrying solve cycle duration")
                .build(),
            solves_executed: meter
                .u64_counter("degenbot.solves.executed")
                .with_description("Solve cycles that carried dirty work")
                .build(),
            registered_paths: meter
                .f64_gauge("degenbot.engine.registered_paths")
                .with_description("Registered solver paths")
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
            simulate_duration: meter
                .f64_histogram("degenbot.simulate.duration")
                .with_unit("s")
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
        }
    }

    /// Header accepted → solve completed.
    pub fn observe_header_to_solved(&self, secs: f64) {
        self.header_to_solved.record(secs, &[]);
    }

    /// Queue time of one drained work item.
    pub fn observe_drain_queue_wait(&self, secs: f64) {
        self.drain_queue_wait.record(secs, &[]);
    }

    /// Decode phase duration.
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

    /// One executed backfill range.
    pub fn count_backfill(&self) {
        self.backfills_executed.add(1, &[]);
    }

    /// Current drain FIFO depth.
    pub fn set_drain_queue_depth(&self, depth: u64) {
        self.drain_queue_depth
            .record(f64::from(u32::try_from(depth).unwrap_or(u32::MAX)), &[]);
    }

    /// Signed `pool_state_head - engine_clock` divergence in blocks. Block
    /// numbers fit far below the f64 mantissa, so the `f64::from` via i32
    /// clamp loses nothing meaningful at block-chain scale.
    pub fn set_state_head_lag(&self, head_minus_clock: i64) {
        let clamped = head_minus_clock.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        self.state_head_lag_blocks
            .record(f64::from(i32::try_from(clamped).unwrap_or_default()), &[]);
    }

    /// One dirty-carrying solve cycle's duration.
    pub fn observe_solve_duration(&self, secs: f64) {
        self.solve_duration.record(secs, &[]);
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
            crate::metrics::try_global_meter().map(|meter| PipelineInstruments::new(&meter))
        })
        .as_ref()
}

#[cfg(test)]
#[expect(clippy::expect_used)] // metric contract asserts loudly
mod kind_tests {
    use opentelemetry::metrics::MeterProvider as _;

    use crate::instruments::PipelineInstruments;
    use crate::telemetry::error_kind;
    use std::collections::HashSet;

    /// The taxonomy is a compile-time closed set: the consts are unique and
    /// are the only values the `kind` label may take.
    #[test]
    fn error_kinds_are_unique() {
        let kinds = [
            error_kind::SOLVER_STATE_DESYNC,
            error_kind::WS_COMPLETENESS,
            error_kind::SIM_FAILURE,
            error_kind::SUBMIT_FAILURE,
            error_kind::MONITOR_FAILURE,
            error_kind::VERIFY_MISMATCH,
            error_kind::DRAIN_STALL,
            error_kind::DRAIN_DEAD,
        ];
        let unique: HashSet<&str> = kinds.iter().copied().collect();
        assert_eq!(unique.len(), kinds.len(), "duplicate failure kind");
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
}
