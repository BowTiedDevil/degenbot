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

/// Register the price-reader pyclasses on the module.
///
/// # Errors
///
/// Returns `PyErr` if a class fails to register on the module.
pub fn add_price_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyChainlinkPriceFeed>()?;
    m.add_class::<PyAavePriceOracle>()?;
    Ok(())
}

use pyo3::prelude::*;
