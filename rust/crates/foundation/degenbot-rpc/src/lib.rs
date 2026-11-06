//! Ethereum RPC provider, contract interface, and subscription core.
//!
//! Pure-Rust crate depending on `degenbot-core` (errors, runtime,
//! `address_utils`), `degenbot-abi` (`abi_types`, `signature_parser`, `encoder`,
//! `decoder`), `alloy` (RPC primitives/provider), `tokio`, and
//! `futures-util`. **No `pyo3`** — the Python-facing wrappers live in the root
//! `degenbot_rs` crate (`provider_py`, `contract_py`, `subscription_py`,
//! `async_provider`, `async_contract`).
//!
//! # Modules
//!
//! - [`provider`] — sync `AlloyProvider`, `LogFetcher`, `LogFilter`, retry logic.
//! - [`fees`] — per-block priority-fee market oracle (`eth_feeHistory` leaf +
//!   the typed p10/p50 parse + `BlockPriorityFees`).
//! - [`contract`] — `Contract` interface with `FunctionSignature` ABI caching.
//! - [`erc20`] — ERC-20 `name()`/`symbol()`/`decimals()` reads: the one
//!   selector/decode/fallback family over [`abi::ContractRead`], with
//!   single-token and multicall call shapes.
//! - [`subscription`] — the one subscription lifecycle
//!   (`SubscriptionHandle::apply` over `SubscriptionPhase`), its double buffer
//!   and `drain_raw`, and the `pump_*` drivers that report into it.
//! - [`head_watch`] — subscription-driven [`HeadWatch`]: the transport half of
//!   the process hub's head source, publishing `newHeads` headers into the hub
//!   with the pump's watchdog/reconnect.

pub mod abi;
pub mod backrun_feed;
pub mod cassette;
pub mod cassette_replay;
pub mod contract;
pub mod erc20;
mod error_class;
mod feed_shell;
pub mod fees;
pub mod head_watch;
pub mod liquidity_verifier;
pub mod multicall3;
pub mod offline;
pub mod pending_tx_stream;
pub mod provider;
mod rate_limit;
mod retry_policy;
pub mod subscription;
pub mod tick_bootstrap;
pub mod txpool_feed;

pub use fees::{fetch_priority_fee_percentiles, BlockPriorityFees};
pub use head_watch::{HeadWatch, HeadWatchConfig, HeadWatchError};
pub use tick_bootstrap::AlloyTickWordSource;
