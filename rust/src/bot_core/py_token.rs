//! `PyToken` — thin Python handle over a token address key into `BotCore`.

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::prelude::*;

use crate::bot_core::BotCore;

/// A thin Python handle to a token registered in `BotCore`.
///
/// Does not own any state — all data lives in Rust inside `BotCore`.
/// Property reads cross `PyO3` on every access.
#[pyclass(name = "Token", skip_from_py_object)]
pub struct PyToken {
    #[allow(dead_code)]
    core: Arc<parking_lot::Mutex<BotCore>>,
    address: Address,
}

impl PyToken {
    /// Create a new thin token handle.
    pub const fn new(core: Arc<parking_lot::Mutex<BotCore>>, address: Address) -> Self {
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
}
