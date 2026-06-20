//! Foundational utilities shared across all degenbot Rust crates.
//!
//! This crate is the dependency-free leaf (modulo `alloy` / `tokio` / `thiserror`)
//! that every other degenbot crate builds on. It has **no `pyo3` dependency under
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
//! - [`runtime`] — shared Tokio runtime singleton.

pub mod address_utils;
pub mod errors;
pub mod hex_utils;
pub mod runtime;
