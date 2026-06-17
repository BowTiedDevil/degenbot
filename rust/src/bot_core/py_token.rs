//! `PyToken` — thin Python handle over a token address key into `Bot`.
//!
//! Shares the same `Arc<parking_lot::RwLock<Bot>>` as the owning `PyBot` (one
//! Rust-owned `Bot`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::prelude::*;

use crate::bot_core::Bot;

/// A thin Python handle to a token registered in `Bot`.
///
/// Does not own any state — all data lives in Rust inside `Bot`.
#[pyclass(skip_from_py_object)]
pub struct PyToken {
    core: Arc<parking_lot::RwLock<Bot>>,
    address: Address,
}

impl PyToken {
    /// Create a new thin token handle.
    pub const fn new(core: Arc<parking_lot::RwLock<Bot>>, address: Address) -> Self {
        Self { core, address }
    }
}

#[pymethods]
impl PyToken {
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
