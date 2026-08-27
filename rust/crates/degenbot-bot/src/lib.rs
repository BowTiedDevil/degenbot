//! The per-chain Rust-owned bot state + Möbius solvers + the unified
//! Uniswap V2/V3/V4 engine, combined into one crate.
//!
//! Per ADR-003, `bot_core` (the `BotState` single-owner state, decoders,
//! reorg journal, verifier, pump) and `solvers` (the Möbius solvers and
//! the `ArbitrageEngine` path/solver/dispatch layer) are a **mutually coupled
//! pair** — ~30 cross-references each way (`BotState` needs
//! `IntHopState`/`IntV3TickRangeSequence`/decoders from `solvers`; the
//! engine needs `BotState`/`V3PoolState`/`TickInfo`/`PoolStateSubscriber`
//! from `bot_core`). ADR-003 explicitly refuses to extract a `LiquidityMap`
//! generic against this sample-of-one, so the two live in one crate here
//! rather than behind an artificial shared-trait seam.
//!
//! This fusion is **tracked debt** (ADR-018): the solve surface is not
//! reachable standalone (a `cargo add degenbot` consumer wanting only the
//! V2/V3/V4 solve math must take this crate + `degenbot-rpc` +
//! `degenbot-db` + `tokio` + `rayon` + `dashmap`). The extraction trigger
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
//! `degenbot_bot::{bot_core, solvers}`.
//!
//! # Modules
//!
//! - [`bot_core`] — `BotState`, decoders, reorg journal, liquidity verifier,
//!   block pump, log/solve/reorg coordinators, V2/V3/V4 state.
//! - [`solvers`] — Möbius solvers + the unified `ArbitrageEngine`.

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
        /// no-op
        pub fn observe_header_to_solved(&self, _secs: f64) {}
        /// no-op
        pub fn observe_drain_queue_wait(&self, _secs: f64) {}
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
        /// no-op
        pub fn count_backfill(&self) {}
        /// no-op
        pub fn set_drain_queue_depth(&self, _depth: u64) {}
        /// no-op
        pub fn set_state_head_lag(&self, _head_minus_clock: i64) {}
        /// no-op
        pub fn set_seconds_since_header(&self, _secs: f64) {}
        /// no-op
        pub fn set_seconds_since_apply(&self, _secs: f64) {}
        /// no-op
        pub fn observe_solve_duration(&self, _secs: f64) {}
        /// no-op
        pub fn count_solves_executed(&self) {}
        /// no-op
        pub fn set_registered_paths(&self, _count: u64) {}
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
    }

    /// Always `None` — metrics are compiled out of this build.
    #[must_use]
    pub fn pipeline() -> Option<&'static PipelineInstruments> {
        None
    }
}
pub mod failure_policy;
#[cfg(feature = "otel")]
pub mod instruments;
#[cfg(feature = "otel")]
pub mod metrics;
#[cfg(feature = "otel")]
pub mod otel;
pub mod profiling;
pub mod solvers;
pub mod telemetry;

/// Configure the process-global rayon pool used by the engine's
/// `par_iter` solve fan-out ([`crate::solvers`]).
///
/// GOQWCL (incident 2026-08-21): the pool was previously implicit — rayon
/// spawns its global pool lazily on first `par_iter` with **unnamed**
/// threads, which masquerade as unrelated workers in thread dumps (during
/// the incident forensics they showed up as anonymous futex waiters and
/// were nearly misattributed to tokio). Naming them makes any future
/// dump immediately attributable.
///
/// Idempotent-ish: if a global pool already exists (another component or
/// test built it first), the request is ignored with a debug log — rayon's
/// global pool can only be configured once per process.
pub fn configure_rayon_solver_pool() {
    let result = rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("degenbot-solve-{i}"))
        .build_global();
    if let Err(err) = result {
        tracing::debug!(
            %err,
            "[rayon] global pool already configured - keeping existing pool"
        );
    }
}
