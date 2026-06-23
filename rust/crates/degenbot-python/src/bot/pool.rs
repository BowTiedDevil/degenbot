//! `PyLiquidityPool` — thin Python handle over a `pool_id` key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `BotState` for reading.

use crate::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

use pyo3::types::{PyDict, PyList};

use crate::bot::journal_err_to_py;
use degenbot_bot::bot_core::{
    BalancerStablePoolState, BalancerWeightedPoolState, BotState, CurvePoolState, TickInfo,
};

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
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_out(self.pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
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
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_in(self.pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
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
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
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
        Ok(crate::conversion::alloy::u256_to_py(py, &r)?.unbind())
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
        Ok(crate::conversion::alloy::u256_to_py(py, &r)?.unbind())
    }

    /// Block number of the most recent state update. Falls through V2→V3→V4
    /// so the same Python companion property works for all families.
    #[getter]
    fn update_block(&self) -> u64 {
        let core = self.core.read();
        if let Some(s) = core.get_v2_pool_state(self.pool_id) {
            return s.update_block;
        }
        // J63J3N: V3 *or* V4 (previously V3-only via get_v3_pool, which
        // returned None for V4 and fell through to 0).
        if let Some(s) = core.get_v3_or_v4_pool(self.pool_id) {
            return s.update_block();
        }
        // Curve: the ADR-005 slice 11a state port. Mirrors V2/V3/V4 — the
        // mutable update_block slot lives in Rust now.
        if let Some(s) = core.get_curve_pool(self.pool_id) {
            return s.update_block;
        }
        // Balancer weighted: the ADR-005 slice 12a state port. Same
        // family-falling-through discipline.
        if let Some(s) = core.get_balancer_weighted_pool(self.pool_id) {
            return s.update_block;
        }
        // Balancer stable: the ADR-005 slice 12c state port. Same
        // family-falling-through discipline.
        if let Some(s) = core.get_balancer_stable_pool(self.pool_id) {
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
                        crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
                        crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
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
            core.get_v3_or_v4_pool(self.pool_id)
                .map(degenbot_bot::bot_core::V3FamilyPool::sqrt_price_x96)
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &spx)?.unbind())
    }

    /// Current active liquidity for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn liquidity(&self) -> u128 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::V3FamilyPool::liquidity)
            .unwrap_or_default()
    }

    /// Current tick for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick(&self) -> i32 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::V3FamilyPool::tick)
            .unwrap_or_default()
    }

    /// Pool fee (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn fee(&self) -> u32 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::V3FamilyPool::fee)
            .unwrap_or_default()
    }

    /// Tick spacing (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick_spacing(&self) -> i32 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::V3FamilyPool::tick_spacing)
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
        let r0 = crate::conversion::alloy::extract_python_u256(reserve0)?;
        let r1 = crate::conversion::alloy::extract_python_u256(reserve1)?;
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
                        crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
                        crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
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
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();
        // RAJ3PP: family-dispatching apply. Routes V4 pools to the V4 apply
        // path (previously this called `apply_v3_swap_by_pool_id`
        // unconditionally, which no-op'd on `PoolEntry::V4` and silently
        // dropped every Python-side V4 update). The dispatcher is one write
        // guard + two O(1) lookups; the single Python `apply_swap` API is
        // preserved.
        let _ = self.core.write().apply_swap_by_pool_id(
            self.pool_id,
            spx,
            liq,
            tick,
            block_number,
            &[],
        );
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
        // liquidity_delta is a signed V3 Mint/Burn delta (Burn events are
        // negative). Extract as i128 directly — V3 deltas fit in i128 (the
        // contract's int128 type). For unusual callers passing values outside
        // i128 range, surface OverflowError rather than silently muffling.
        let delta: i128 = liquidity_delta.extract().map_err(|_| {
            pyo3::exceptions::PyOverflowError::new_err(
                "liquidity_delta must fit in i128 (V3 contract int128 range)",
            )
        })?;
        let applied = self.core.write().apply_liquidity_update_by_pool_id(
            self.pool_id,
            tick_lower,
            tick_upper,
            delta,
            block_number,
        );
        Ok(applied.is_some())
    }

    /// Replace this pool's `tick_data` with an external snapshot (Python
    /// sparse-map backfill). Mirrors the Python `UniswapV3Pool.update_tick_data`
    /// — the companion delegates here once it's rewritten over the handle
    /// (plan-101 slice 8b). No journal delta (full-sync; the pump is the
    /// authority for event-derived ticks — mirrors `sync_v3_pool_state`).
    ///
    /// `tick_data` is the SAME shape `tick_data_snapshot` returns:
    /// `{tick: (liquidity_gross, liquidity_net, block)}` — the write path is
    /// symmetric with the read path, + the companion converts its
    /// `LiquidityAtTick` objects to this tuple shape at the boundary (matching
    /// how V2 converts its Python `Fraction` fees to the Rust `gamma_numer` at
    /// the boundary). The `tick_bitmap` dict is REDUNDANT in Rust (the bitmap
    /// is derived from `tick_data` keys — see `tick_bitmap_snapshot`);
    /// accepted for API parity with the Python caller signature, ignored.
    ///
    /// Scalars (`sqrt_price_x96`/`liquidity`/`tick`) are UNCHANGED — this is
    /// tick-only. `update_block` advances to `block` if newer (monotonic).
    ///
    /// Returns `True` if the replace applied to a registered V3/V4 pool,
    /// `False` if this `pool_id` is a V2 pool or unregistered (silent no-op —
    /// mirrors the `apply_liquidity_update` family contract).
    #[pyo3(signature = (tick_bitmap, tick_data, block))]
    fn update_tick_data(
        &self,
        tick_bitmap: &Bound<'_, PyAny>,
        tick_data: &Bound<'_, PyDict>,
        block: u64,
    ) -> PyResult<bool> {
        let _ = tick_bitmap; // redundant in Rust (derived from tick_data keys)
        let mut map: HashMap<i32, TickInfo> = HashMap::with_capacity(tick_data.len());
        for (key, value) in tick_data.iter() {
            let tick: i32 = key.extract().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "update_tick_data: tick_data keys must be ints",
                )
            })?;
            // Symmetric with `tick_data_snapshot`: a 3-tuple
            // `(liquidity_gross, liquidity_net, block)` (the block is the
            // Symmetric with `tick_data_snapshot`: a 3-tuple
            // `(liquidity_gross, liquidity_net, block)`. The per-tick block is
            // preserved on the Rust ``TickInfo.block`` field (mirrors the
            // Python ``LiquidityAtTick.block`` — the snapshot round-trip's
            // per-tick block contract).
            let (gross, net, tick_block): (u128, i128, u64) = value.extract().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "update_tick_data: tick_data values must be (gross, net, block) tuples",
                )
            })?;
            map.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: alloy::primitives::I256::try_from(net)
                        .unwrap_or(alloy::primitives::I256::ZERO),
                    block: tick_block,
                },
            );
        }
        Ok(self
            .core
            .write()
            .sync_tick_data_by_pool_id(self.pool_id, map, block))
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
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(None);
            };
            (
                s.sqrt_price_x96(),
                s.liquidity(),
                s.tick(),
                s.update_block(),
            )
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &snap.0)?.unbind(),
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
        // J63J3N: only apply when this handle points at a V3/V4 pool —
        // otherwise silently no-op (avoids corrupting a V2's journal from a
        // V3-shaped companion). The family dispatcher routes V4 to its own
        // journal method; V2 / unregistered no-ops (V3's contract).
        if core.get_v3_or_v4_pool(self.pool_id).is_none() {
            return Ok(());
        }
        core.discard_v3_or_v4_before_block(self.pool_id, block)
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
            if core.get_v3_or_v4_pool(self.pool_id).is_none() {
                return Ok(None);
            }
            core.restore_v3_or_v4_before_block(self.pool_id, block)
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
                        crate::conversion::alloy::u256_to_py(py, &p.sqrt_price_x96_before)?
                            .unbind(),
                        p.liquidity_before.into_pyobject(py)?.into_any().unbind(),
                        p.tick_before.into_pyobject(py)?.into_any().unbind(),
                        restore.block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }

    /// Snapshot of the V3/V4 `tick_data` `HashMap` as a Python dict.
    ///
    /// Returns `{tick: (liquidity_gross, liquidity_net, block)}` — the Python
    /// companion's `tick_data` property lifts each row into an immutable
    /// `LiquidityAtTick(liquidity_net, liquidity_gross, block)`. Rust's
    /// `TickInfo` stores `liquidity_gross`, `liquidity_net`, and `block`
    /// (the block at which the tick was last mutated — mirrors the Python
    /// ``LiquidityAtTick.block`` field).
    ///
    /// Returns an empty dict if this `pool_id` is not registered as a V3/V4
    /// pool (defensive — non-V3 callers shouldn't crash).
    #[pyo3(signature = ())]
    fn tick_data_snapshot(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rows: Vec<(i32, (u128, i128, u64))> = {
            let core = self.core.read();
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(pyo3::types::PyDict::new(py).into_any().unbind());
            };
            s.tick_data()
                .iter()
                .map(|(tick, info)| {
                    let net: i128 = i128::try_from(info.liquidity_net).unwrap_or(0);
                    let gross: u128 = info.liquidity_gross.to::<u128>();
                    (*tick, (gross, net, info.block))
                })
                .collect()
        };
        let dict = pyo3::types::PyDict::new(py);
        for (tick, (gross, net, block)) in rows {
            let row = pyo3::types::PyTuple::new(
                py,
                [
                    gross.into_pyobject(py)?.into_any().unbind(),
                    net.into_pyobject(py)?.into_any().unbind(),
                    block.into_pyobject(py)?.into_any().unbind(),
                ],
            )?;
            dict.set_item(tick, row)?;
        }
        Ok(dict.into_any().unbind())
    }

    /// Snapshot of the V3/V4 tick bitmap, synthesized from `tick_data` keys.
    ///
    /// Rust's `V3PoolState` doesn't store a separate bitmap — the bitmap is
    /// derivable from `tick_data` keys (initialized ticks). Returns
    /// `{word_pos: (bitmap_int, block)}` where `word_pos = (tick //
    /// tick_spacing) >> 8` and the bit set is `(tick // tick_spacing) % 256`.
    /// Matches Solidity `TickBitmap.position(tick / tickSpacing)`.
    ///
    /// Returns an empty dict for non-V3/V4 `pool_ids`.
    #[pyo3(signature = ())]
    fn tick_bitmap_snapshot(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        // Collect (word_pos, bit_pos, block) per initialized tick under one read
        // guard, then build the output dict without holding the lock.
        let rows: Vec<(i32, u32, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(pyo3::types::PyDict::new(py).into_any().unbind());
            };
            let spacing = s.tick_spacing().max(1);
            let update_block = s.update_block();
            // U256 (256 bits per word) — `u128` would overflow for bit positions
            // ≥ 128 (large ticks land in high bits). Mirrors Solidity's uint256.
            s.tick_data()
                .keys()
                .map(|&tick| {
                    let compressed = tick / spacing;
                    (
                        compressed >> 8,
                        compressed.rem_euclid(256) as u32,
                        update_block,
                    )
                })
                .collect()
        };
        // Fold bits into per-word (bitmap_int, block) accumulators.
        let one = alloy::primitives::U256::from(1u64);
        let mut words: std::collections::BTreeMap<i32, (alloy::primitives::U256, u64)> =
            std::collections::BTreeMap::new();
        for (word_pos, bit_pos, block) in rows {
            words
                .entry(word_pos)
                .and_modify(|(bits, _)| *bits |= one << bit_pos)
                .or_insert((one << bit_pos, block));
        }
        let dict = pyo3::types::PyDict::new(py);
        for (word_pos, (bits, block)) in words {
            let tuple = pyo3::types::PyTuple::new(
                py,
                [
                    crate::conversion::alloy::u256_to_py(py, &bits)?.unbind(),
                    block.into_pyobject(py)?.into_any().unbind(),
                ],
            )?;
            dict.set_item(word_pos, tuple)?;
        }
        Ok(dict.into_any().unbind())
    }

    // --- Curve state read getters + mutations (ADR-005 slice 11a state port) ---

    /// Number of tokens for a Curve pool (`balances.len()`).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Curve pool.
    #[getter]
    fn n_coins(&self) -> usize {
        self.core
            .read()
            .get_curve_pool(self.pool_id)
            .map_or(0, CurvePoolState::n_coins)
    }

    /// Current balances for a Curve pool (one `U256` per token).
    ///
    /// Returns `None` if this `pool_id` is not registered as a Curve pool
    /// (so a V2/V3/V4 companion built for a different family doesn't crash).
    #[getter]
    fn balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// Snapshot a Curve pool's mutable state as `(balances, update_block)`.
    ///
    /// Returns `None` for non-Curve pools (the V3/V4 `snapshot_v3` family
    /// analogue — family-dispatching readers).
    #[pyo3(signature = ())]
    fn snapshot_curve(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Apply a Curve `external_update` (new balances from an `Exchange` event).
    ///
    /// Journals the prior balances then lands the new balances + `update_block`.
    /// Silent no-op (`False`) if this `pool_id` is not registered as a Curve
    /// pool (so a V2/V3/V4 companion doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_curve_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_curve_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }

    // --- Balancer weighted state read getters + mutations
    //     (ADR-005 slice 12a state port) ---

    /// Token count for a Balancer weighted pool (`balances.len()`).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer weighted pool.
    #[getter]
    fn n_balancer_tokens(&self) -> usize {
        self.core
            .read()
            .get_balancer_weighted_pool(self.pool_id)
            .map_or(0, BalancerWeightedPoolState::n_tokens)
    }

    /// Current balances for a Balancer weighted pool (one `U256` per token).
    ///
    /// Returns an empty list if this `pool_id` is not registered as a
    /// Balancer weighted pool (so a V2/V3/V4/Curve companion built for a
    /// different family doesn't crash).
    #[getter]
    fn balancer_balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_weighted_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// Snapshot a Balancer weighted pool's mutable state as
    /// `(balances, update_block)`.
    ///
    /// Returns `None` for non-Balancer-weighted pools (the family-
    /// dispatching reader analogue to `snapshot_curve` / `snapshot_v3`).
    #[pyo3(signature = ())]
    fn snapshot_balancer_weighted(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_weighted_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Apply a Balancer weighted `external_update` (new balances from a Vault
    /// `PoolBalanceChanged` event).
    ///
    /// Journals the prior balances then lands the new balances +
    /// `update_block`. Silent no-op (`False`) if this `pool_id` is not
    /// registered as a Balancer weighted pool (so a V2/V3/V4/Curve companion
    /// doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_balancer_weighted_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_balancer_weighted_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }

    // --- Balancer stable state read getters + mutations
    //     (ADR-005 slice 12c state port) ---

    /// Token count for a Balancer stable pool (`balances.len()` — includes
    /// BPT for Composable pools).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn n_balancer_stable_tokens(&self) -> usize {
        self.core
            .read()
            .get_balancer_stable_pool(self.pool_id)
            .map_or(0, BalancerStablePoolState::n_tokens)
    }

    /// Current balances for a Balancer stable pool (one `U256` per token,
    /// including BPT for Composable pools).
    ///
    /// Returns an empty list if this `pool_id` is not registered as a Balancer
    /// stable pool (so a companion built for a different family doesn't crash).
    #[getter]
    fn balancer_stable_balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_stable_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// BPT token index for a Balancer stable pool: `None` for `MetaStablePools`,
    /// `Some(i)` for `ComposableStablePools`.
    ///
    /// Returns `None` if this `pool_id` is not registered as a Balancer stable
    /// pool (also a valid value for a registered `MetaStable` — see the
    /// `invariant_version` getter to distinguish).
    #[getter]
    fn balancer_bpt_index(&self) -> Option<usize> {
        self.core
            .read()
            .get_balancer_stable_pool(self.pool_id)
            .and_then(|s| s.bpt_idx)
    }

    /// Amplification coefficient `amp` for a Balancer stable pool (immutable
    /// after registration in this plan — A ramping is a future, non-epic
    /// concern resolved by the builder at registration).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_amp(&self) -> u128 {
        self.core
            .read()
            .get_balancer_stable_pool(self.pool_id)
            .map_or(0, |s| s.amp)
    }

    /// `invariant_version` discriminator (1 = V1 always-roundDown `D_P`
    /// accumulation; 2 = V2 roundUp-param `P_D` accumulation) — the
    /// systematic-1-wei-error guard.
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_invariant_version(&self) -> u8 {
        self.core
            .read()
            .get_balancer_stable_pool(self.pool_id)
            .map_or(0, |s| s.invariant_version)
    }

    /// Snapshot a Balancer stable pool's mutable state as
    /// `(balances, update_block)`.
    ///
    /// Returns `None` for non-Balancer-stable pools (the family-dispatching
    /// reader analogue to `snapshot_curve` / `snapshot_balancer_weighted`).
    #[pyo3(signature = ())]
    fn snapshot_balancer_stable(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_stable_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Apply a Balancer stable `external_update` (new balances from a Vault
    /// `PoolBalanceChanged` event).
    ///
    /// Journals the prior balances then lands the new balances +
    /// `update_block`. Silent no-op (`False`) if this `pool_id` is not
    /// registered as a Balancer stable pool (so a companion built for a
    /// different family doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_balancer_stable_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_balancer_stable_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }
}
