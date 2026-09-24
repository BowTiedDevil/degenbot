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
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.price")?;
    submod.add_class::<PyChainlinkPriceFeed>()?;
    submod.add_class::<PyAavePriceOracle>()?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.price", &submod)?;
    Ok(())
}

use pyo3::prelude::*;
use pyo3::types::PyModule;
