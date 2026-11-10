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
//! - [`errors`] — centralized error types (`TickMathError`, `ConcentratedLiquidityMathError`,
//!   `AbiDecodeError`, `AddressError`, `ProviderError`, `ContractError`) with
//!   inter-error conversions. The `From<Error> for PyErr` impls are gated behind
//!   the `pyo3` feature.
//! - [`fee`] — the V2 fee vocabulary: one validated type per fee convention
//!   (the fee fraction vs the retained gamma), with typed refusals.
//! - [`hex_utils`] — pure-Rust hex encoding/decoding.
//! - [`address_utils`] — EIP-55 checksummed Ethereum addresses.
//! - [`cpu_budget`] — cgroup-aware CPU budget detection and the two-runtime
//!   (solve bins / ambient I/O) sizing policy.
//! - [`runtime`] — shared Tokio runtime singleton, sized by [`cpu_budget`].
//! - [`pubsub_runtime`] — the owned dedicated-pubsub-runtime type: the one
//!   home of the deterministic teardown discipline (build a named single-worker
//!   runtime, register it, drive it across a grace on drop) plus live-instance
//!   census counting.
//! - [`worker_census`] — boot-time registry of every execution resource
//!   (name/kind/count/thread-name/sizing) exported as the
//!   `degenbot_worker_census` gauge ; NEW SPAWN SITES MUST
//!   REGISTER — see the module docs.
//! - [`eip_1559`] — EIP-1559 `next_base_fee` (next-block base fee).
//! - [`fee_percentiles`] — the shared priority-fee percentile pair the RPC
//!   oracle polls and the settlement-arbitrage strategy sizes against.
//! - [`session_positions`] — the position seam: session-canonical position
//!   identity, the refusal vocabulary a read answers with, and the observer
//!   trait a lending integration implements. It sits here because the engine and
//!   the lending integrations already share this layer and no other one.
//! - [`chain_facts`] — the intrinsic chain-facts manifest: one native asset's
//!   name/symbol/decimals per chain, resolved by chain id. Metadata only —
//!   native-token identity and the protocol-private sentinel translations
//!   live at their own layers.
//! - [`channel_lifecycle`] — the one-way delivery channel's phase. The phase IS
//!   the sender, so "never attached" and "closed" cannot collapse into one
//!   `None`; `SendOutcome` names which way a send did not deliver.
//! - [`canonical_head`] — the accepted canonical head's identity (number, hash,
//!   parent hash) and the one-method sink a consumer installs to observe it.

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
