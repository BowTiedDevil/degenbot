//! # `degenbot-price` — on-chain price readers (ADR-005 core leaf)
//!
//! A pyo3-free core crate owning the **price MECHANISM**: typed readers for
//! on-chain price feeds that route `eth_call` through the already-Rust-owned
//! [`degenbot_rpc::contract::Contract::call_typed`].
//!
//! Two readers:
//!
//! - [`chainlink::ChainlinkPriceFeed`] — ports `ChainlinkPriceContract`
//!   (`latestRoundData()` + `decimals()` + decimal-corrected `price()`).
//! - [`aave::AavePriceOracle`] — ports `OraclePriceFetcher`
//!   (`getAssetPrice(address)` + a tolerant `fetch_prices` batch).
//!
//! # Standalone-core constraint (ADR-005)
//!
//! A standalone Rust consumer `cargo add degenbot` can value positions using
//! these readers with **zero Python, zero `pyo3`** (verified by
//! `just check-no-pyo3-in-cores`). This crate owns the feed-*reading*
//! mechanism; the *consumer* valuation orchestration (ranking paths by USD
//! value, per-token `_price_oracle` plumbing on `Erc20Token`) stays Python
//! until its own port epic — that is out of scope here.
//!
//! # Parity (ADR-005 §4.2)
//!
//! The Python `ChainlinkPriceContract` / `OraclePriceFetcher` are the parity
//! oracles. The §4.2 gates pin value-exact parity for the `latestRoundData`
//! tuple decode, the `decimals` `uint8` decode, the `price()` decimal
//! correction, and the Aave `getAssetPrice` `uint256` decode — driven from
//! recorded EVM return bytes (canonical ABI decode, which is what the Python
//! `abi_decode` performs too).
//!
//! # Non-goals
//!
//! - The `PyO3` seam + Python cutover (sibling task).
//! - Reading the oracle address from the DB (the Aave rehydrate path,
//!   `OKKMG5`) — this crate **consumes** a resolved `Address`.
//! - Valuation/ranking orchestration using the prices (stays-Python).
//!
//! [`degenbot_rpc::contract::Contract::call_typed`]: ../degenbot_rpc/contract/struct.Contract.html#method.call_typed

pub mod aave;
pub mod chainlink;
pub(crate) mod decode;
pub mod error;

// ---- Convenience top-level re-exports (mirrors the umbrella convention) ----

pub use aave::AavePriceOracle;
pub use chainlink::{ChainlinkPriceFeed, RoundData};
pub use error::{PriceError, PriceResult};
