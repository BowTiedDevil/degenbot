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
//!
//! # Modules
//!
//! - [`errors`] — centralized error types (`TickMathError`, `ClMathError`,
//!   `AbiDecodeError`, `AddressError`, `ProviderError`, `ContractError`) with
//!   inter-error conversions. The `From<Error> for PyErr` impls are gated behind
//!   the `pyo3` feature.
//! - [`hex_utils`] — pure-Rust hex encoding/decoding.
//! - [`address_utils`] — EIP-55 checksummed Ethereum addresses.
//! - [`cpu_budget`] — cgroup-aware CPU budget detection and the two-runtime
//!   (solve bins / ambient I/O) sizing policy.
//! - [`runtime`] — shared Tokio runtime singleton, sized by [`cpu_budget`].
//! - [`worker_census`] — boot-time registry of every execution resource
//!   (name/kind/count/thread-name/sizing) exported as the
//!   `degenbot_worker_census` gauge (ergo PE4FPM); NEW SPAWN SITES MUST
//!   REGISTER — see the module docs.
//! - [`eip_1559`] — EIP-1559 `next_base_fee` (next-block base fee).

pub mod address_utils;
pub mod block_clock_pipe;
pub mod cpu_budget;
pub mod eip_1559;
pub mod errors;
pub mod hex_utils;
pub mod libzip;
pub mod retry;
pub mod runtime;
pub mod telemetry;
pub mod worker_census;
