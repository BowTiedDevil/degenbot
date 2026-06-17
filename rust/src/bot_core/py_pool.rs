//! `PyPool` — thin Python handle over a `pool_id` key into `Bot`.
//!
//! Shares the same `Arc<RwLock<Bot>>` as the owning `PyBot` (Polars-style:
//! one Rust-owned `Bot`, many thin Python handles).
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `Bot` for reading.

use std::sync::Arc;

use pyo3::prelude::*;

use crate::bot_core::Bot;

/// Encode a byte slice as a lowercase hex string (no "0x" prefix).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    s
}

/// A thin Python handle to a pool registered in `Bot`.
///
/// Does not own any state — all data lives in Rust inside `Bot`.
#[pyclass(name = "Pool", skip_from_py_object)]
pub struct PyPool {
    core: Arc<parking_lot::RwLock<Bot>>,
    pool_id: u64,
}

impl PyPool {
    /// Create a new thin pool handle.
    pub const fn new(core: Arc<parking_lot::RwLock<Bot>>, pool_id: u64) -> Self {
        Self { core, pool_id }
    }

    /// The pool ID this handle references.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.pool_id
    }
}

#[pymethods]
impl PyPool {
    /// The pool ID this handle references.
    #[getter]
    #[allow(clippy::missing_const_for_fn)]
    fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// Calculate the output token amount for a given input amount.
    #[pyo3(signature = (zero_for_one, amount_in))]
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::alloy_py::extract_python_u256(amount_in)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_out(self.pool_id, zero_for_one, amount)
        };
        let bound = crate::alloy_py::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Calculate the required input token amount for a given output amount.
    #[pyo3(signature = (zero_for_one, amount_out))]
    fn calculate_tokens_in(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::alloy_py::extract_python_u256(amount_out)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_in(self.pool_id, zero_for_one, amount)
        };
        let bound = crate::alloy_py::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Encode a V2 swap call, returning `(to_address_hex, calldata_hex, value)`.
    #[pyo3(signature = (zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        recipient: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        let amount = crate::alloy_py::extract_python_u256(amount_out)?;
        let recip = match recipient.parse() {
            Ok(addr) => addr,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid address '{recipient}': {e}"
                )));
            }
        };

        let result = {
            let core = self.core.read();
            core.encode_swap(self.pool_id, zero_for_one, amount, recip)
        };

        Ok(result.map(|call| {
            let to_hex = format!("{:#x}", call.to);
            let data_hex = format!("0x{}", bytes_to_hex(&call.data));
            (to_hex, data_hex, call.value.to::<u64>())
        }))
    }
}
