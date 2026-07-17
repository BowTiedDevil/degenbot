//! Ethereum RPC provider, contract interface, and subscription core.
//!
//! Pure-Rust crate depending on `degenbot-core` (errors, runtime,
//! `address_utils`), `degenbot-abi` (`abi_types`, `signature_parser`, `abi_encoder`,
//! `abi_decoder`), `alloy` (RPC primitives/provider), `tokio`, and
//! `futures-util`. **No `pyo3`** — the Python-facing wrappers live in the root
//! `degenbot_rs` crate (`provider_py`, `contract_py`, `subscription_py`,
//! `async_provider`, `async_contract`).
//!
//! # Modules
//!
//! - [`provider`] — sync `AlloyProvider`, `LogFetcher`, `LogFilter`, retry logic.
//! - [`contract`] — `Contract` interface with `FunctionSignature` ABI caching.
//! - [`subscription`] — double-buffer `SubscriptionHandle`, raw `drain_raw`,
//!   and the `pump_*` subscription drivers.

pub mod abi;
pub mod contract;
pub mod multicall3;
pub mod offline;
pub mod provider;
pub mod subscription;
pub mod tick_bootstrap;

pub use tick_bootstrap::AlloyTickBootstrapRpc;
