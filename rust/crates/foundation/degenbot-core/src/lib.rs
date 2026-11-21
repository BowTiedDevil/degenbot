//! Foundational utilities shared across all degenbot Rust crates.
//!
//! This crate is the foundational leaf every other degenbot crate builds on.
//! Its only non-ecosystem dependencies are `degenbot-config` (the typed
//! `BotConfig` schema the CPU-budget sizing keys live in) and `parking_lot`.
//! It has **no `pyo3` dependency under
//! default features** — the optional `pyo3` feature enables `From<Error> for PyErr`
//! conversions co-located with the error types (the orphan rule forbids putting
//! those impls in a downstream crate, since both `PyErr` and the error types would
//! be foreign there).

pub mod address_utils;
pub mod block_notification;
pub mod canonical_head;
pub mod chain_facts;
pub mod channel_lifecycle;
pub mod cpu_budget;
pub mod eip_1559;
pub mod errors;
pub mod fee;
pub mod fee_percentiles;
pub mod hex_utils;
pub mod libzip;
pub mod log_capture;
pub mod pubsub_runtime;
pub mod retry;
pub mod runtime;
pub mod session_positions;
pub mod telemetry;
pub mod updater_telemetry;
pub mod worker_census;
