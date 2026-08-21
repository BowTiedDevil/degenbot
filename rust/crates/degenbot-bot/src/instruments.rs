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
    /// Per-path EVM simulation duration.
    simulate_duration: Histogram<f64>,
    /// Simulation outcomes, labeled by verdict string.
    simulate_verdicts: Counter<u64>,
}

impl PipelineInstruments {
    /// Build all instruments from a meter. Visible for tests — production
    /// callers go through [`pipeline`].
    #[must_use]
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
            simulate_duration: meter
                .f64_histogram("degenbot.simulate.duration")
                .with_unit("s")
                .with_description("Per-path EVM simulation duration")
                .build(),
            simulate_verdicts: meter
                .u64_counter("degenbot.simulate.verdicts")
                .with_description("Simulation outcomes by verdict")
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
}

static PIPELINE: OnceLock<Option<PipelineInstruments>> = OnceLock::new();

/// The process-wide instrument set, or `None` while metrics are disabled
/// (otel feature off is compiled out entirely; gate off / not yet initialized
/// lands here). Idempotent — cheap to call per observation.
#[must_use]
pub fn pipeline() -> Option<&'static PipelineInstruments> {
    PIPELINE
        .get_or_init(|| {
            crate::metrics::try_global_meter().map(|meter| {
                let instruments = PipelineInstruments::new(&meter);
                // Touch one instrument so an empty scrape is distinguishable
                // from "instruments never registered" in Grafana.
                instruments
                    .blocks_observed
                    .add(0, &[KeyValue::new("init", "true")]);
                instruments
            })
        })
        .as_ref()
}
