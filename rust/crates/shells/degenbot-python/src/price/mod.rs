//! `PyO3` seam over the `degenbot-price` core crate.
//!
//! Wraps [`degenbot_price::ChainlinkPriceFeed`] and
//! [`degenbot_price::AavePriceOracle`] as `#[pyclass]` types so the Python
//! companion `ChainlinkPriceContract` / `OraclePriceFetcher` shells delegate
//! to the Rust `eth_call` + ABI decode path. The wrappers hold no business
//! logic: arg extraction → `py.detach()` the RPC `eth_call` → wrap the typed
//! return (ADR-005 §3 PyO3-layer discipline).
//!
//! The `eth_call` is the only async boundary — it is driven via
//! [`runtime::get_runtime().block_on`] inside [`Python::detach`], mirroring
//! `crate::rpc::contract::PyContract::call` (sync Python callers; the price
//! path is non-hot — read per valuation sweep, not per block).

pub mod aave;
pub mod chainlink;

pub use aave::PyAavePriceOracle;
pub use chainlink::PyChainlinkPriceFeed;

/// The `degenbot._ffi.price` Python submodule (declarative `#[pymodule]`),
/// carrying the price-reader pyclasses. The parent module registers the
/// submodule itself and its `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod price {
    #[pymodule_export]
    use super::{PyAavePriceOracle, PyChainlinkPriceFeed};
}

use pyo3::prelude::*;
