//! # degenbot-fork
//!
//! Pure-Rust Anvil fork lifecycle + dev-RPC core.
//!
//! Owns the embedded [`anvil`](https://book.getfoundry.sh/reference/anvil/)
//! node lifecycle (spawn the `anvil` binary as a subprocess via
//! [`alloy::node_bindings::Anvil`]), the alloy `Provider` wired to it over
//! IPC, and the anvil dev-RPC surface (`evm_mine`, `anvil_reset`,
//! `anvil_snapshot`, `anvil_revert`, `anvil_set_balance`, `anvil_set_code`,
//! `anvil_set_nonce`, `anvil_set_storage_at`, `anvil_set_next_block_base_fee_per_gas`,
//! `anvil_set_next_block_timestamp`, `anvil_set_block_timestamp_interval`,
//! `anvil_set_coinbase`) as typed Rust methods delegating to the provider's
//! [`alloy::providers::ext::AnvilApi`] trait.
//!
//! This crate is a standalone-Rust core: a Rust-only consumer can
//! `cargo add degenbot-fork` to spin an anvil fork — no Python required. The
//! `anvil` binary must be present in `$PATH` at runtime (it is the same
//! runtime dependency degenbot already required for the legacy Python
//! `AnvilFork`).
//!
//! Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`). The `PyO3` wrapper
//! lives under `degenbot-python/src/fork/`.

pub mod node;
