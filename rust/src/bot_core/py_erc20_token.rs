//! `PyErc20Token` — thin Python handle over a token address key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::prelude::*;

use crate::bot_core::BotState;

/// A thin Python handle to a token registered in `BotState`.
///
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(skip_from_py_object)]
pub struct PyErc20Token {
    core: Arc<parking_lot::RwLock<BotState>>,
    address: Address,
}

impl PyErc20Token {
    /// Create a new thin token handle.
    pub(crate) const fn new(core: Arc<parking_lot::RwLock<BotState>>, address: Address) -> Self {
        Self { core, address }
    }
}

#[pymethods]
impl PyErc20Token {
    /// The token contract address (hex string).
    #[getter]
    fn address(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let addr_str = format!("{}", self.address);
        Ok(addr_str.into_pyobject(py)?.into_any().unbind())
    }

    /// Token decimals (e.g. 6 for USDC, 18 for WETH).
    #[getter]
    fn decimals(&self) -> PyResult<u8> {
        let core = self.core.read();
        let Some(entry) = core.token_entry(&self.address) else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "token not registered: {}",
                self.address
            )));
        };
        Ok(entry.decimals)
    }

    /// Chain ID where this token is registered.
    #[getter]
    fn chain_id(&self) -> PyResult<u64> {
        let core = self.core.read();
        let Some(entry) = core.token_entry(&self.address) else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "token not registered: {}",
                self.address
            )));
        };
        Ok(entry.chain_id)
    }

    /// Token symbol (e.g. "WETH", "USDC").
    #[getter]
    fn symbol(&self) -> PyResult<String> {
        let core = self.core.read();
        let Some(entry) = core.token_entry(&self.address) else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "token not registered: {}",
                self.address
            )));
        };
        Ok(entry.symbol.clone())
    }

    /// Token name (e.g. "Wrapped Ether").
    #[getter]
    fn name(&self) -> PyResult<String> {
        let core = self.core.read();
        let Some(entry) = core.token_entry(&self.address) else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "token not registered: {}",
                self.address
            )));
        };
        Ok(entry.name.clone())
    }
}
