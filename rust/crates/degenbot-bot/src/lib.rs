//! The per-chain Rust-owned bot state + the unified Uniswap V2/V3/V4
//! arbitrage engine, combined into one crate.
//!
//! Per ADR-003, `bot_core` (the `BotState` single-owner state, decoders,
//! reorg journal, verifier, pump) and `arb_engine` (the `ArbitrageEngine`
//! path/solve/dispatch + delivery layer) are a **mutually coupled pair** —
//! ~30 cross-references each way (`BotState` needs the solver value types
//! `IntHopState`/`IntV3TickRangeSequence` from `degenbot-solvers` and the
//! decoders; the engine needs `BotState`/`V3PoolState`/`TickInfo`
//! from `bot_core`). ADR-003 explicitly refuses to
//! extract a `LiquidityMap` generic against this sample-of-one, so the two
//! live in one crate here rather than behind an artificial shared-trait
//! seam.
//!
//! This fusion is **tracked debt** (ADR-018): the solve surface is not
//! reachable standalone (a `cargo add degenbot` consumer wanting only the
//! V2/V3/V4 solve math must take this crate + `degenbot-rpc` +
//! `degenbot-db` + `tokio` + `dashmap`). The extraction trigger
//! is a **second engine family** joining (e.g. an `AaveLiquidationEngine`
//! or a split `SolidlyEngine`); until then, the cross-references are the
//! cost of one engine family and one state owner co-evolving.
//!
//! # `PyO3` boundary
//!
//! The pure core (this crate's default features) has **no `pyo3` dependency**.
//! The `#[pyclass]`/`#[pyfunction]` bindings (`PyBot`, `PyLiquidityPool`,
//! `PyErc20Token`, `PyDexIdentity`, `PyArbitrageEngine`, the
//! `Verification*Error`/`*RejectedError` exception types) live in the root
//! `degenbot_rs` cdylib's `py_bot` / `py_liquidity_pool` / `py_erc20_token` /
//! `py_dex_identity` / `py_binding` modules — they need `conversion::alloy` /
//! `conversion::cache` (binding-layer concerns). They reach the pure core through
//! `degenbot_bot::{arb_engine, bot_core}`.
//!
//! # Modules
//!
//! - [`bot_core`] — `BotState`, decoders, reorg journal, liquidity verifier,
//!   block pump, log/solve/reorg coordinators, V2/V3/V4 state.
//! - [`arb_engine`] — the unified multi-DEX `ArbitrageEngine`: per-block
//!   lifecycle, path registry, solve dispatch, inline simulation, delivery.
//!
//! The stateless solver math itself (Möbius composition, V3/V4 integer
//! tick-range solving, the `QuantAMM` Balancer basket solver) lives in the
//! `degenbot-solvers` crate and is imported via `::degenbot_solvers` —
//! never re-exported from here.

pub mod bot_core;

/// Default-build stub for the instruments module: same call surface, always
/// `None`. Observation sites stay ungated (`if let Some(p) = pipeline()`) so
/// the hot path reads identically in both builds — the compiler drops the
/// branch, and default builds compile zero metrics code.
#[cfg(not(feature = "otel"))]
pub mod instruments {
    /// Inert twin of the real instrument set; never constructed.
    #[derive(Debug)]
    pub struct PipelineInstruments;

    impl PipelineInstruments {
        /// no-op (ADR-041 epoch race)
        pub fn observe_header_to_publish(&self, _secs: f64) {}
        /// no-op (ADR-041 streaming stage wait)
        pub fn observe_streaming_age(&self, _secs: f64) {}
        /// no-op (ADR-041 I3 stale drops)
        pub fn count_stale_drop(&self) {}
        /// no-op
        pub fn observe_header_to_first_log(&self, _secs: f64) {}
        /// no-op
        pub fn observe_log_burst(&self, _secs: f64) {}
        /// no-op
        pub fn observe_settle_wait(&self, _secs: f64) {}
        /// no-op
        pub fn observe_log_decode(&self, _secs: f64) {}
        /// no-op
        pub fn observe_state_apply(&self, _secs: f64) {}
        /// no-op
        pub fn count_block(&self) {}
        /// no-op
        pub fn count_log_received(&self) {}
        /// no-op
        pub fn count_log_applied(&self) {}
        /// no-op
        pub fn count_log_apply_missed(&self) {}
        /// no-op (WAJEQP T-R1)
        pub fn count_reorg_window(&self) {}
        /// no-op (WAJEQP T-R1)
        pub fn count_reorg_unwound_pool(&self) {}
        /// no-op (WAJEQP T-R1)
        pub fn observe_reorg_depth(&self, _blocks: u64) {}
        /// no-op (WAJEQP T-R1)
        pub fn count_reorg_recovery_dropped(&self) {}
        /// no-op (benign late-admit family)
        pub fn count_late_log_admitted(&self) {}
        /// no-op (BM35LK adaptive quiesce window)
        pub fn observe_quiesce_window(&self, _ms: u64) {}
        /// no-op
        pub fn count_ws_log_seen(&self) {}
        /// no-op
        pub fn count_log_decoded(&self) {}
        /// no-op
        pub fn count_log_undecoded(&self) {}
        /// no-op
        pub fn count_solver_verify_block(&self) {}
        /// no-op
        pub fn count_backfill(&self) {}
        /// no-op
        pub fn observe_cgroup_throttled(&self, _events_delta: u64, _usecs_delta: u64) {}
        /// no-op
        pub fn observe_state_lock_wait(&self, _site: &str, _mode: &str, _secs: f64) {}
        /// no-op
        pub fn observe_state_lock_hold(&self, _site: &str, _mode: &str, _secs: f64) {}
        /// no-op (ADR-040)
        pub fn set_quarantined_pools(&self, _count: usize) {}
        /// no-op (ADR-040)
        pub fn count_sim_error_reason(&self, _reason: &str) {}
        /// no-op
        pub fn set_process_rss_bytes(&self, _bytes: u64) {}
        /// no-op (NO4DIW per-block log funnel)
        pub fn observe_epoch_logs(
            &self,
            _seen: u64,
            _received: u64,
            _applied: u64,
            _ignored: u64,
            _closing_block: u64,
        ) {
        }
        /// no-op
        pub fn observe_publish_cycle(&self, _secs: f64) {}
        /// no-op
        pub fn count_rewind(&self) {}
        /// no-op
        pub fn observe_rewind_duration(&self, _secs: f64) {}
        /// no-op
        pub fn set_state_head_lag(&self, _head_minus_clock: i64) {}
        /// no-op
        pub fn set_seconds_since_header(&self, _secs: f64) {}
        /// no-op
        pub fn set_seconds_since_apply(&self, _secs: f64) {}
        /// no-op
        pub fn observe_mutex_hold_duration(&self, _secs: f64, _arm: &'static str) {}
        /// no-op
        pub fn observe_solve_duration(&self, _secs: f64, _arm: &'static str) {}
        /// no-op
        pub fn observe_per_path_solve_duration(&self, _secs: f64) {}
        /// no-op
        pub fn observe_per_path_gate_duration(&self, _secs: f64) {}
        /// no-op
        pub fn count_solves_executed(&self) {}
        /// no-op
        pub fn set_registered_paths(&self, _count: u64) {}
        /// no-op (TB4QGX T7)
        pub fn set_intake_backlog(&self, _role: &str, _depth: u64) {}
        /// no-op
        pub fn count_candidates_found(&self, _n: u64) {}
        /// no-op
        pub fn observe_simulate_duration(&self, _secs: f64) {}
        /// no-op
        pub fn count_simulate_verdict(&self, _verdict: &str) {}
        /// no-op
        pub fn observe_dispatch_profits(&self, _gross_wei: f64, _net_wei: f64) {}
        /// no-op
        pub fn observe_dispatch_gas(&self, _gas: u64) {}
        /// no-op
        pub fn count_submit_outcome(&self, _outcome: &str) {}
        /// no-op (PRG-2 registration-skip family)
        pub fn count_registration_skip(&self, _reason: &str) {}
        /// no-op
        pub fn observe_submit_latency(&self, _secs: f64) {}
        /// no-op
        pub fn add_profit_realized(&self, _wei: f64) {}
        /// no-op
        pub fn add_profit_missed(&self, _wei: f64) {}
        /// no-op
        pub fn count_monitor_outcome(&self, _outcome: &str) {}
        /// no-op
        pub fn count_clamp(&self) {}
        /// no-op
        pub fn count_error(&self, _kind: &'static str) {}
        /// no-op
        pub fn count_solver_state_check(&self) {}
        /// no-op
        pub fn set_detached_in_flight(&self, _count: u64) {}
        /// no-op
        pub fn count_detached_stale_dropped(&self) {}
        /// no-op
        pub fn count_detached_applied(&self) {}
        /// no-op (cold-start trace: degraded-cycle counter)
        pub fn count_detached_degraded_cycle(&self) {}
        /// no-op (AQV6EF: detached outcomes lost to a dead merge drain)
        pub fn count_detached_send_failed(&self) {}
        /// no-op (AQV6EF: merge-seat panics caught by the sidecar guard)
        pub fn count_detached_merge_panic(&self) {}
        /// no-op (QTZGFL: admission-shed cycles)
        pub fn count_detached_shed(&self) {}
        /// no-op (QTZGFL: retained admission keys expired by the retention window)
        pub fn count_detached_leads_expired(&self, _n: u64) {}
    }

    /// Close-out: resident set bytes (drift-watch). Default
    /// builds return None — the otel twin reads /proc/self/statm.
    #[must_use]
    pub fn read_process_rss_bytes() -> Option<u64> {
        None
    }

    /// no-op (FF-T5 fleet profile metric)
    pub fn note_fleet_profile(_summary: &crate::arb_engine::fleet_status::FleetProfileSummary) {}

    /// Always `None` — metrics are compiled out of this build.
    #[must_use]
    pub fn pipeline() -> Option<&'static PipelineInstruments> {
        None
    }
}
pub mod allocator_ctrl;
pub mod arb_engine;
/// The PRG-3 intake surface (LNQDOA): the documented single re-export the
/// pyo3 leaf depends on — no other `arb_engine` module is public surface.
pub use arb_engine::fleet_intake;
pub mod failure_policy;
#[cfg(feature = "otel")]
pub mod instruments;
#[cfg(feature = "otel")]
pub mod metrics;
pub mod nonce_authority;
#[cfg(feature = "otel")]
pub mod otel;
pub mod profiling;
pub mod sidecar;
pub mod sidecar_engine;
pub mod sidecar_paths;
pub mod strategy_host;
pub mod telemetry;

// P6YXA6 hard cutover: the process-global rayon pool (`configure_rayon_
// solver_pool`) is retired with the rayon dispatch arms it served — every
// solve bin rides the fleet-hosted executor or the dedicated private tokio
// runtime, and the resolve fan-out runs on scoped std threads. The
// two-runtime contract stands: the ambient I/O runtime keeps the headroom
// cores; the solve-executor bins are sized from `solve_worker_count()` and
// its leftover, never from raw `available_parallelism`.
