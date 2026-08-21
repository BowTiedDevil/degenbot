//! `PyErc20Token` — thin Python handle over a token address key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use crate::prelude::*;
use std::sync::Arc;

use alloy::primitives::Address;

use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_bot::bot_core::BotState;

/// A thin Python handle to a token registered in `BotState`.
///
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(name = "Erc20Token", skip_from_py_object, module = "degenbot._ffi")]
pub struct PyErc20Token {
    core: Arc<StateLock<BotState>>,
    address: Address,
}

impl PyErc20Token {
    /// Create a new thin token handle.
    pub(crate) const fn new(core: Arc<StateLock<BotState>>, address: Address) -> Self {
        Self { core, address }
    }

    /// Sanctioned `BotState` read access for pymethod code (GIL/`BotState`
    /// inversion class): the guard is acquired INSIDE `py.detach`. Same
    /// invariant contract as `PyBot::with_state` — see the doc comment there.
    pub(crate) fn with_state<T>(&self, py: Python<'_>, f: impl FnOnce(&BotState) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(|| {
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let guard = self.core.read();
            f(&guard)
        })
    }
}

/// Build the `token not registered` error. Must run under the GIL (`PyErr`
/// construction touches Python); call it AFTER the detached scope returns.
fn token_not_registered(addr: &Address) -> pyo3::PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(format!("token not registered: {addr}"))
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
    ///
    /// GIL hygiene (GIL/`BotState` inversion class): the `BotState` read guard is
    /// acquired inside `py.detach`; owned data comes out and any `PyErr` is
    /// built after the GIL is re-acquired.
    #[getter]
    fn decimals(&self, py: Python<'_>) -> PyResult<u8> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.decimals)
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Chain ID where this token is registered.
    #[getter]
    fn chain_id(&self, py: Python<'_>) -> PyResult<u64> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.chain_id)
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Token symbol (e.g. "USDC", "WETH").
    #[getter]
    fn symbol(&self, py: Python<'_>) -> PyResult<String> {
        self.with_state(py, |s| {
            s.token_entry(&self.address)
                .map(|entry| entry.symbol.clone())
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Token name (e.g. "Wrapped Ether").
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.name.clone())
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }
}
