//! `PyLiquidityPool` — thin Python handle over a `pool_id` key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `BotState` for reading.

use std::sync::Arc;

use pyo3::prelude::*;

use crate::bot_core::py_bot::journal_err_to_py;
use crate::bot_core::BotState;

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

/// A thin Python handle to a pool registered in `BotState`.
///
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(skip_from_py_object)]
pub struct PyLiquidityPool {
    core: Arc<parking_lot::RwLock<BotState>>,
    pool_id: u64,
}

impl PyLiquidityPool {
    /// Create a new thin pool handle.
    pub(crate) const fn new(core: Arc<parking_lot::RwLock<BotState>>, pool_id: u64) -> Self {
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
    // These read the shared `BotState` under a read guard. Immutable identity
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

    /// Block number of the most recent state update. Falls through V2→V3→V4
    /// so the same Python companion property works for all families.
    #[getter]
    fn update_block(&self) -> u64 {
        let core = self.core.read();
        if let Some(s) = core.get_v2_pool_state(self.pool_id) {
            return s.update_block;
        }
        if let Some(s) = core.get_v3_pool(self.pool_id) {
            return s.update_block;
        }
        0
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

    // --- V3 state read getters (plan-101 slice 8a) ---
    // Mirror the V2 family but read the V3PoolState entry. All getters take
    // one read guard and return None-defaulted values when the pool_id is not
    // a registered V3 pool (matching the V2 getters' behavior on V2).

    /// Current `sqrt_price_x96` (Q64.96) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn sqrt_price_x96(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let spx = {
            let core = self.core.read();
            core.get_v3_pool(self.pool_id)
                .map(|s| s.sqrt_price_x96)
                .unwrap_or_default()
        };
        Ok(crate::alloy_py::u256_to_py(py, &spx)?.unbind())
    }

    /// Current active liquidity for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn liquidity(&self) -> u128 {
        self.core
            .read()
            .get_v3_pool(self.pool_id)
            .map(|s| s.liquidity)
            .unwrap_or_default()
    }

    /// Current tick for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick(&self) -> i32 {
        self.core
            .read()
            .get_v3_pool(self.pool_id)
            .map(|s| s.tick)
            .unwrap_or_default()
    }

    /// Pool fee (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn fee(&self) -> u32 {
        self.core
            .read()
            .get_v3_pool(self.pool_id)
            .map(|s| s.fee)
            .unwrap_or_default()
    }

    /// Tick spacing (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick_spacing(&self) -> i32 {
        self.core
            .read()
            .get_v3_pool(self.pool_id)
            .map(|s| s.tick_spacing)
            .unwrap_or_default()
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

    // --- V3 mutations (plan-101 slice 8a) ---
    // Pool-id-keyed — the handle already holds the canonical pool_id, so no
    // address resolution is needed (single lock, single lookup).

    /// Apply a V3/V4 `Swap` event: journals the scalar priors then lands the
    /// new `sqrt_price_x96`/`liquidity`/`tick` at `block_number`.
    ///
    /// Swap events change the V3 scalars but NOT the tick data — the
    /// `tick_priors` Vec is empty here (unlike `PyBot.update_v3_pool`, which
    /// accepts tick updates from decoded Swap logs when they carry tick
    /// mutations).
    ///
    /// Raises:
    ///     `ValueError`: If `pool_id` is not registered as a V3/V4 pool.
    #[pyo3(signature = (sqrt_price_x96, liquidity, tick, block_number))]
    fn apply_swap(
        &self,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let spx = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::alloy_py::extract_python_u256(liquidity)?.to::<u128>();
        let _ = self
            .core
            .write()
            .apply_v3_swap_by_pool_id(self.pool_id, spx, liq, tick, block_number, &[]);
        Ok(())
    }

    /// Apply a V3 Mint/Burn event (liquidity update) via the handle.
    ///
    /// Initializes (or removes) tick entries at `tick_lower`/`tick_upper`,
    /// journals the priors for reorg rollback, invalidates the tick-range
    /// cache. Does NOT change the V3 scalars (`sqrt_price_x96`/`liquidity`/
    /// `tick`) — Mint/Burn is a tick-only event per ADR-004. The active
    /// `liquidity` scalar adjustments (when `current_tick` is in range) are
    /// applied by the engine's own path; this handle method is the raw
    /// `tick_data` mutation.
    ///
    /// Returns `True` if the update applied to a registered V3 pool, `False`
    /// if this `pool_id` is not a V3 pool (silent no-op — don't corrupt a V2
    /// pool).
    #[pyo3(signature = (tick_lower, tick_upper, liquidity_delta, block_number))]
    fn apply_liquidity_update(
        &self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<bool> {
        // liquidity_delta is i128 — accept Python int, narrow from i256.
        let delta_i256 = crate::alloy_py::extract_python_u256(liquidity_delta)?;
        let delta = alloy::primitives::I256::from_raw(delta_i256)
            .try_into()
            .map_err(|_| {
                pyo3::exceptions::PyOverflowError::new_err(
                    "liquidity_delta does not fit in i128",
                )
            })?;
        let applied = self
            .core
            .write()
            .apply_v3_liquidity_update_by_pool_id(
                self.pool_id,
                tick_lower,
                tick_upper,
                delta,
                block_number,
            );
        Ok(applied.is_some())
    }

    /// Atomic V3/V4 scalar snapshot: `(sqrt_price_x96, liquidity, tick, block)`.
    ///
    /// All four fields are read under ONE read guard (the same atomicity
    /// contract as V2 `snapshot()`). The Python companion's `state` property
    /// builds a `UniswapV3PoolState` from this single tuple — no torn reads.
    ///
    /// Returns `None` if this `pool_id` is not registered as a V3/V4 pool.
    #[pyo3(signature = ())]
    fn snapshot_v3(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap = {
            let core = self.core.read();
            let Some(s) = core.get_v3_pool(self.pool_id) else {
                return Ok(None);
            };
            (
                s.sqrt_price_x96,
                s.liquidity,
                s.tick,
                s.update_block,
            )
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::alloy_py::u256_to_py(py, &snap.0)?.unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
                snap.2.into_pyobject(py)?.into_any().unbind(),
                snap.3.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Discard V3/V4 reorg journal deltas earlier than `block`.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the
    /// target is past the newest delta. Silent no-op when this `pool_id` is not
    /// a registered V3/V4 pool (so a Python companion built for V3 doesn't
    /// touch V2/V4 journal state).
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (block))]
    fn discard_v3_before_block(&self, block: u64) -> PyResult<()> {
        let mut core = self.core.write();
        // Only apply when this handle points at a V3/V4 pool — otherwise
        // silently no-op (avoids corrupting a V2's journal from a V3-shaped
        // companion).
        if core.get_v3_pool(self.pool_id).is_none() {
            return Ok(());
        }
        core.v3_discard_before_block(self.pool_id, block)
            .map_err(journal_err_to_py)
    }

    /// Restore a V3/V4 pool to the landed-at state just before `block`.
    ///
    /// Returns `(sqrt_price_x96, liquidity, tick, block)` as Python ints, or
    /// `None` if this `pool_id` is not registered as a V3/V4 pool.
    ///
    /// Raises:
    ///     `ValueError`: If `block` is at or before the registration block.
    #[pyo3(signature = (block))]
    fn restore_v3_before_block(&self, py: Python<'_>, block: u64) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let mut core = self.core.write();
            if core.get_v3_pool(self.pool_id).is_none() {
                return Ok(None);
            }
            core.v3_restore_before_block(self.pool_id, block)
        };
        match result {
            None => Ok(None),
            Some(restore) => {
                let p = restore
                    .scalar_priors
                    .as_ref()
                    .expect("post-restore scalar_priors must be Some");
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::alloy_py::u256_to_py(py, &p.sqrt_price_x96_before)?.unbind(),
                        p.liquidity_before.into_pyobject(py)?.into_any().unbind(),
                        p.tick_before.into_pyobject(py)?.into_any().unbind(),
                        restore.block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }
}
