//! `PyLiquidityPool` — thin Python handle over a `pool_id` key into `Bot`.
//!
//! Shares the same `Arc<parking_lot::RwLock<Bot>>` as the owning `PyBot` (one
//! Rust-owned `Bot`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `Bot` for reading.

use std::sync::Arc;

use pyo3::prelude::*;

use crate::bot_core::py_bot::journal_err_to_py;
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
#[pyclass(skip_from_py_object)]
pub struct PyLiquidityPool {
    core: Arc<parking_lot::RwLock<Bot>>,
    pool_id: u64,
}

impl PyLiquidityPool {
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
impl PyLiquidityPool {
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

    // --- State read getters (ADR-005 slice 4 step 2) ---
    // These read the shared `Bot` under a read guard. Immutable identity
    // (token0/token1/factory/fees/address) stays on the Python companion —
    // only mutable state + the reorg journal delegate to Rust.

    /// Current reserve of token0.
    #[getter]
    fn reserve0(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = {
            let core = self.core.read();
            core.get_v2_pool_state(self.pool_id)
                .map(|s| s.reserve0)
                .unwrap_or_default()
        };
        Ok(crate::alloy_py::u256_to_py(py, &r)?.unbind())
    }

    /// Current reserve of token1.
    #[getter]
    fn reserve1(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = {
            let core = self.core.read();
            core.get_v2_pool_state(self.pool_id)
                .map(|s| s.reserve1)
                .unwrap_or_default()
        };
        Ok(crate::alloy_py::u256_to_py(py, &r)?.unbind())
    }

    /// Block number of the most recent state update.
    #[getter]
    fn update_block(&self) -> u64 {
        self.core
            .read()
            .get_v2_pool_state(self.pool_id)
            .map(|s| s.update_block)
            .unwrap_or_default()
    }

    /// Atomic snapshot of (reserve0, reserve1, `update_block`) under one read guard.
    ///
    /// The companion's `state` property + `simulate_*` methods build their
    /// state object from this single snapshot so a Rust-side `sync_reserves`
    /// (pump update) can't interleave between separate `reserve0`/`reserve1`
    /// reads (replaces the `StateCache.lock()` atomicity the drop-`StateCache`
    /// refactor loses). Returns `None` if the pool isn't registered or isn't a
    /// V2 pool.
    #[pyo3(signature = ())]
    fn snapshot(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap = { self.core.read().v2_snapshot(self.pool_id) };
        match snap {
            None => Ok(None),
            Some((r0, r1, blk)) => {
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::alloy_py::u256_to_py(py, &r0)?.unbind(),
                        crate::alloy_py::u256_to_py(py, &r1)?.unbind(),
                        blk.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }

    // --- Mutations (per-handle, pool_id-keyed) ---

    /// Apply a V2 `Sync` event: journals the prior reserves then lands the new.
    /// Equivalent to `PyBot.update_v2_pool(address, ...)` but keyed by the
    /// handle's `pool_id` (no address resolution, single lock).
    #[pyo3(signature = (reserve0, reserve1, block_number))]
    fn sync_reserves(
        &self,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;
        let _ = self
            .core
            .write()
            .apply_v2_sync_by_pool_id(self.pool_id, r0, r1, block_number);
        Ok(())
    }

    /// Number of deltas in the V2 reorg journal (genesis + transitions).
    fn journal_len(&self) -> usize {
        self.core.read().v2_journal_len(self.pool_id)
    }

    /// Discard V2 reorg journal deltas earlier than `block`.
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta (would remove
    ///         every known state).
    #[pyo3(signature = (block))]
    fn discard_before_block(&self, block: u64) -> PyResult<()> {
        self.core
            .write()
            .v2_discard_before_block(self.pool_id, block)
            .map_err(journal_err_to_py)
    }

    /// Restore the V2 pool to the landed-at state just before `block`.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None` if the
    /// pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If `block` is at or before the registration block.
    #[pyo3(signature = (block))]
    fn restore_before_block(&self, py: Python<'_>, block: u64) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let mut core = self.core.write();
            core.v2_restore_before_block(self.pool_id, block)
        };
        match result {
            None => Ok(None),
            Some(Err(e)) => Err(journal_err_to_py(e)),
            Some(Ok((r0, r1, blk))) => {
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::alloy_py::u256_to_py(py, &r0)?.unbind(),
                        crate::alloy_py::u256_to_py(py, &r1)?.unbind(),
                        blk.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }
}
